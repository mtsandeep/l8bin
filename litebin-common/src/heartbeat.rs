use std::collections::HashSet;
use std::time::Duration;

use bollard::container::LogOutput;
use futures_util::StreamExt;
use tracing::{debug, info, warn};

use crate::docker::DockerManager;

/// Default flush interval in seconds.
pub const FLUSH_INTERVAL_SECS: u64 = 60;

/// Build the Caddy `logging` JSON config block that writes access logs to stdout.
/// Uses the built-in `stdout` writer — no custom Caddy modules needed.
pub fn caddy_logging_config() -> serde_json::Value {
    serde_json::json!({
        "logging": {
            "logs": {
                "default": {
                    "writer": {
                        "output": "stdout"
                    },
                    "encoder": {
                        "format": "json"
                    }
                }
            }
        }
    })
}

/// Minimal view of a Caddy JSON access log line — only `request.host` is
/// materialized; every other field is skipped by serde without allocating.
/// The borrowed lifetimes keep the host as a `&str` slice of the input line.
#[derive(serde::Deserialize)]
struct CaddyAccessLine<'a> {
    #[serde(borrow)]
    request: Option<CaddyRequestLine<'a>>,
}

#[derive(serde::Deserialize)]
struct CaddyRequestLine<'a> {
    host: Option<&'a str>,
}

/// Extract the host from a Caddy JSON access log line.
/// Caddy's JSON encoder nests the host under `request.host`.
fn extract_host_from_line(line: &str) -> Option<&str> {
    let parsed: CaddyAccessLine = serde_json::from_str(line).ok()?;
    let host = parsed.request?.host?;
    if host.is_empty() {
        return None;
    }
    // Strip port if present
    Some(host.split(':').next().unwrap_or(host))
}

/// Append raw log bytes to `buffer` and harvest hosts from any lines the
/// bytes complete. Lines are processed as slices of `buffer` and drained in
/// place; the only steady-state allocation is one `String` per unique host.
fn harvest_hosts(buffer: &mut String, bytes: &[u8], hosts: &mut HashSet<String>) {
    buffer.push_str(&String::from_utf8_lossy(bytes));
    while let Some(newline_pos) = buffer.find('\n') {
        let line = buffer[..newline_pos].trim();
        if let Some(host) = extract_host_from_line(line)
            && !hosts.contains(host)
        {
            hosts.insert(host.to_string());
        }
        buffer.drain(..newline_pos + 1);
    }
}

/// Run an activity tracker that tails Docker container logs,
/// collects unique hosts from Caddy access logs,
/// and periodically calls `on_flush` with the collected hosts.
///
/// Reconnects automatically if the log stream breaks (e.g., container restart).
/// Exits gracefully when `shutdown_rx` signals shutdown.
pub async fn run_docker_log_tailer<F, Fut>(
    docker: DockerManager,
    container_name: String,
    flush_interval_secs: u64,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    on_flush: F,
) where
    F: Fn(HashSet<String>) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!(container = %container_name, "activity tracker: shutting down");
                break;
            }
            _ = async {} => {}
        }

        let since = chrono::Utc::now().timestamp();
        info!(container = %container_name, since = since, "activity tracker: creating log stream");

        let stream = docker.follow_container_logs(&container_name, Some(since));

        info!(container = %container_name, "activity tracker: tailing container logs");

        match tail_stream(stream, flush_interval_secs, &on_flush, shutdown_rx.clone()).await {
            Ok(()) => {
                info!(container = %container_name, "activity tracker: log stream ended");
            }
            Err(e) => {
                warn!(
                    container = %container_name,
                    error = %e,
                    "activity tracker: log stream error, reconnecting in 10s"
                );
            }
        }

        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!(container = %container_name, "activity tracker: shutting down");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}

/// Process a single log stream, collecting hosts and flushing periodically.
async fn tail_stream<S, F, Fut>(
    mut stream: S,
    flush_interval_secs: u64,
    on_flush: &F,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()>
where
    S: StreamExt<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
    F: Fn(HashSet<String>) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut hosts: HashSet<String> = HashSet::new();
    let mut buffer = String::new();
    let mut interval = tokio::time::interval(Duration::from_secs(flush_interval_secs));

    loop {
        tokio::select! {
            result = stream.next() => {
                match result {
                    Some(Ok(log_output)) => {
                        let bytes = match log_output {
                            LogOutput::StdOut { message } => message,
                            LogOutput::StdErr { message } => message,
                            _ => continue,
                        };
                        harvest_hosts(&mut buffer, &bytes, &mut hosts);
                    }
                    Some(Err(e)) => {
                        return Err(e.into());
                    }
                    None => {
                        return Err(anyhow::anyhow!("log stream ended"));
                    }
                }
            }
            _ = interval.tick() => {
                if hosts.is_empty() {
                    continue;
                }
                let batch: HashSet<String> = std::mem::take(&mut hosts);
                let count = batch.len();
                debug!(host_count = count, "activity tracker: flushing hosts");
                on_flush(batch).await;
            }
            _ = shutdown_rx.changed() => {
                // Flush remaining hosts before exiting
                if !hosts.is_empty() {
                    let batch: HashSet<String> = std::mem::take(&mut hosts);
                    on_flush(batch).await;
                }
                return Err(anyhow::anyhow!("shutdown"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access_line(host: &str) -> String {
        format!(
            r#"{{"level":"info","ts":1755000000.0,"logger":"http.log.access","request":{{"remote_ip":"10.0.0.1","method":"GET","host":"{host}","uri":"/"}},"duration":0.001}}"#
        )
    }

    #[test]
    fn extracts_host_from_access_line() {
        assert_eq!(extract_host_from_line(&access_line("app.example.com")), Some("app.example.com"));
    }

    #[test]
    fn strips_port_from_host() {
        assert_eq!(extract_host_from_line(&access_line("app.example.com:8443")), Some("app.example.com"));
    }

    #[test]
    fn rejects_non_access_lines() {
        assert_eq!(extract_host_from_line(r#"{"level":"info","msg":"serving initial configuration"}"#), None);
        assert_eq!(extract_host_from_line("not json at all"), None);
        assert_eq!(extract_host_from_line(r#"{"request":{}}"#), None);
        assert_eq!(extract_host_from_line(r#"{"request":{"host":""}}"#), None);
    }

    #[test]
    fn harvests_only_complete_lines_and_dedupes() {
        let mut buffer = String::new();
        let mut hosts = HashSet::new();

        // Two complete lines (same host — second must not re-allocate an entry)
        // plus a trailing partial line that must stay buffered.
        let chunk = format!("{}\n{}\n{{\"req", access_line("a.example.com"), access_line("a.example.com"));
        harvest_hosts(&mut buffer, chunk.as_bytes(), &mut hosts);
        assert_eq!(hosts, HashSet::from(["a.example.com".to_string()]));
        assert_eq!(buffer, "{\"req");

        // Completing the partial line via a later chunk harvests its host.
        let rest = "uest\":{\"host\":\"b.example.com\"}}\n";
        harvest_hosts(&mut buffer, rest.as_bytes(), &mut hosts);
        assert_eq!(hosts, HashSet::from(["a.example.com".to_string(), "b.example.com".to_string()]));
        assert_eq!(buffer, "");
    }

    #[test]
    fn repeated_traffic_grows_nothing() {
        // Per-request processing must not allocate: only unique hosts ever
        // allocate an entry.
        let mut buffer = String::new();
        let mut hosts = HashSet::new();
        for _ in 0..1000 {
            harvest_hosts(&mut buffer, format!("{}\n", access_line("same.example.com")).as_bytes(), &mut hosts);
        }
        assert_eq!(hosts.len(), 1);
        assert_eq!(buffer, "");
    }
}
