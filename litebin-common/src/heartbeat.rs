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

/// Extract the host from a Caddy JSON access log line.
/// Caddy's JSON encoder nests the host under `request.host`.
fn extract_host_from_line(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let host = v.get("request")?.get("host")?.as_str()?;
    if host.is_empty() {
        return None;
    }
    // Strip port if present
    Some(host.split(':').next().unwrap_or(host).to_string())
}

/// Run an activity tracker that tails Docker container logs,
/// collects unique hosts from Caddy access logs,
/// and periodically calls `on_flush` with the collected hosts.
///
/// Reconnects automatically if the log stream breaks (e.g., container restart).
pub async fn run_docker_log_tailer<F, Fut>(
    docker: DockerManager,
    container_name: String,
    flush_interval_secs: u64,
    on_flush: F,
) where
    F: Fn(HashSet<String>) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        let since = chrono::Utc::now().timestamp();
        info!(container = %container_name, since = since, "activity tracker: creating log stream");

        let stream = docker.follow_container_logs(&container_name, Some(since));

        info!(container = %container_name, "activity tracker: tailing container logs");

        match tail_stream(stream, flush_interval_secs, &on_flush).await {
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

        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// Process a single log stream, collecting hosts and flushing periodically.
async fn tail_stream<S, F, Fut>(
    mut stream: S,
    flush_interval_secs: u64,
    on_flush: &F,
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
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        // Process complete lines
                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();
                            if let Some(host) = extract_host_from_line(&line) {
                                hosts.insert(host);
                            }
                        }
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
        }
    }
}
