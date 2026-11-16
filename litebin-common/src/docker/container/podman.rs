//! CRIU checkpoint/restore via podman's native libpod API — checkpoint/restore
//! has no Docker API equivalent, so bollard cannot express it. Served on the
//! same socket as the Docker-compatible API.

use std::time::Duration;

#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixStream;

use super::super::DockerManager;
use super::super::RuntimeKind;

/// Checkpoint of a large app dumps its whole memory to disk.
const CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(120);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(30);

/// How a container was put to sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepOutcome {
    Checkpointed,
    Stopped,
}

/// How a sleeping container was woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    Restored,
    Started,
}

/// POST a libpod endpoint over the podman socket. Sends `Connection: close`
/// so the response can be read to EOF without parsing transfer framing.
/// Returns (status_code, body).
#[cfg(unix)]
async fn podman_post(socket: &str, path: &str, timeout: Duration) -> anyhow::Result<(u16, String)> {
    let request = format!("POST {path} HTTP/1.1\r\nHost: podman\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");

    let mut stream =
        UnixStream::connect(socket).await.map_err(|e| anyhow::anyhow!("podman socket {socket} unreachable: {e}"))?;

    let response: Vec<u8> = tokio::time::timeout(timeout, async {
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .map_err(|_| anyhow::anyhow!("podman {path} timed out after {}s", timeout.as_secs()))?
    .map_err(|e| anyhow::anyhow!("podman {path} IO error: {e}"))?;

    parse_http_response(&String::from_utf8_lossy(&response))
}

/// Podman's Unix socket only exists on Linux nodes; the Windows build of the
/// crate just refuses checkpoint/restore calls.
#[cfg(not(unix))]
async fn podman_post(_socket: &str, path: &str, _timeout: Duration) -> anyhow::Result<(u16, String)> {
    anyhow::bail!("podman {path} requires a Unix host (this binary was built for Windows)")
}

/// Parse "HTTP/1.1 200 OK" status line and split the body from the headers.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_http_response(response: &str) -> anyhow::Result<(u16, String)> {
    let status_line = response.lines().next().unwrap_or_default();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("malformed podman response status: {}", status_line.trim_end()))?;
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    Ok((code, body))
}

/// Best-effort error message from a libpod error body (`{"cause": "…"}`).
fn podman_error(action: &str, container_id: &str, code: u16, body: &str) -> anyhow::Error {
    let cause = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("cause").and_then(|c| c.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());
    anyhow::anyhow!("podman {action} {container_id} failed ({code}): {cause}")
}

impl DockerManager {
    /// True when this node can CRIU checkpoint/restore (podman runtime).
    pub fn supports_checkpoint(&self) -> bool {
        self.runtime == RuntimeKind::Podman
    }

    /// CRIU-checkpoint a running container (podman only). Both flags are
    /// required for real servers: `tcpEstablished` (keep-alive connections)
    /// and `fileLocks` (embedded DBs like H2/SQLite).
    async fn podman_checkpoint(&self, container_id: &str) -> anyhow::Result<()> {
        let socket = self.podman_socket.as_deref().ok_or_else(|| anyhow::anyhow!("podman socket not configured"))?;
        let path = format!("/v4.0.0/libpod/containers/{container_id}/checkpoint?tcpEstablished=true&fileLocks=true");
        let (code, body) = podman_post(socket, &path, CHECKPOINT_TIMEOUT).await?;
        if (200..300).contains(&code) { Ok(()) } else { Err(podman_error("checkpoint", container_id, code, &body)) }
    }

    /// Restore a checkpointed container (podman only) — no app boot. Flags
    /// must mirror the checkpoint side.
    async fn podman_restore(&self, container_id: &str) -> anyhow::Result<()> {
        let socket = self.podman_socket.as_deref().ok_or_else(|| anyhow::anyhow!("podman socket not configured"))?;
        let path = format!("/v4.0.0/libpod/containers/{container_id}/restore?tcpEstablished=true&fileLocks=true");
        let (code, body) = podman_post(socket, &path, RESTORE_TIMEOUT).await?;
        if (200..300).contains(&code) { Ok(()) } else { Err(podman_error("restore", container_id, code, &body)) }
    }

    /// Put a container to sleep. With `checkpoint: true` on a podman node the
    /// container is CRIU-checkpointed (state preserved, wake skips app boot);
    /// any checkpoint failure falls back to a plain stop so sleep never fails.
    pub async fn sleep_container(&self, container_id: &str, checkpoint: bool) -> anyhow::Result<SleepOutcome> {
        if checkpoint && self.supports_checkpoint() {
            match self.podman_checkpoint(container_id).await {
                Ok(()) => {
                    tracing::info!(container_id = %container_id, "container checkpointed (CRIU)");
                    return Ok(SleepOutcome::Checkpointed);
                }
                Err(e) => {
                    tracing::warn!(container_id = %container_id, error = %e, "checkpoint failed, falling back to stop");
                }
            }
        }
        self.stop_container(container_id).await?;
        Ok(SleepOutcome::Stopped)
    }

    /// Wake a sleeping container. On podman, a CRIU restore is attempted first
    /// (for plain-stopped containers the restore fails fast and start takes
    /// over); on docker this is exactly `start_existing_container`.
    pub async fn wake_container(
        &self,
        container_id: &str,
        service_name: &str,
        is_oneshot: bool,
    ) -> anyhow::Result<WakeOutcome> {
        if self.supports_checkpoint() {
            match self.podman_restore(container_id).await {
                Ok(()) => {
                    tracing::info!(container_id = %container_id, "container restored from checkpoint");
                    // Brief yield so port mappings settle before callers probe.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(WakeOutcome::Restored);
                }
                Err(e) => {
                    tracing::debug!(container_id = %container_id, error = %e, "restore not applicable, starting container");
                }
            }
        }
        self.start_existing_container(container_id, service_name, is_oneshot).await?;
        Ok(WakeOutcome::Started)
    }

    /// Sleep the current primary container selected by project/service identity,
    /// checkpointing when supported. Idempotent when the container is absent.
    pub async fn sleep_primary_service_container(
        &self,
        project_id: &str,
        service_name: &str,
        checkpoint: bool,
    ) -> anyhow::Result<bool> {
        let name = crate::types::primary_service_container_name(project_id, service_name)
            .ok_or_else(|| anyhow::anyhow!("invalid project/service identity"))?;
        let Some(container_id) = self.find_container_by_name(&name).await? else {
            return Ok(false);
        };
        self.sleep_container(&container_id, checkpoint).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}";
        let (code, body) = parse_http_response(resp).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{}");
    }

    #[test]
    fn parses_error_response() {
        let resp = "HTTP/1.1 500 Internal Server Error\r\n\r\n{\"cause\":\"is not checkpointed\"}";
        let (code, body) = parse_http_response(resp).unwrap();
        assert_eq!(code, 500);
        let err = podman_error("restore", "abc", code, &body).to_string();
        assert!(err.contains("is not checkpointed"), "got: {err}");
    }

    #[test]
    fn malformed_status_is_an_error() {
        assert!(parse_http_response("garbage").is_err());
        assert!(parse_http_response("").is_err());
    }
}
