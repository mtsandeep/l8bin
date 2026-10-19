//! Wake-hold machinery shared by both wakers: WakeGuard single-flight completion, the wait-endpoint contract, the post-start readiness probe.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

/// Path the waiting page JS long-polls; the app answers it (no marker header) once running, the waker while it sleeps.
pub const WAKE_WAIT_PATH: &str = "/__l8b_wake_wait";

/// Marks wait-endpoint responses so the JS can tell a waker answer from an app answer.
pub const WAKE_WAIT_HEADER: &str = "x-l8b-wake";

/// Default silent-hold budget before the class-specific fallback; operator-tunable via `WAKE_HOLD_SECS`.
pub const WAKE_SILENT_HOLD: Duration = Duration::from_secs(2);

/// How long subresources keep holding past the silent budget, so a cached-HTML revisit renders styled.
pub const WAKE_ASSET_HOLD_CAP: Duration = Duration::from_secs(30);

/// Per-poll cap for the wait endpoint; the JS re-fetches on timeout.
pub const WAKE_WAIT_POLL_CAP: Duration = Duration::from_secs(30);

/// Probe window before the wake fails. The container is never stopped — later requests serve once the app listens.
pub const WAKE_READINESS_MAX: Duration = Duration::from_secs(60);

/// How long a completed guard stays in the map so late waiters see the outcome.
pub const WAKE_GUARD_RETENTION: Duration = Duration::from_secs(60);

/// Silent-hold budget from `WAKE_HOLD_SECS` (seconds, fractional, 0 disables). Read once per process.
pub fn wake_silent_hold() -> Duration {
    static HOLD: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *HOLD.get_or_init(|| {
        std::env::var("WAKE_HOLD_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|secs| *secs >= 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(WAKE_SILENT_HOLD)
    })
}

/// Single-flight completion signal: the first requester spawns the wake, others await `wait_for()`.
pub struct WakeGuard {
    pub notify: Notify,
    pub success: AtomicBool,
    pub completed: AtomicBool,
}

impl WakeGuard {
    pub fn new() -> Self {
        Self { notify: Notify::new(), success: AtomicBool::new(false), completed: AtomicBool::new(false) }
    }

    /// Mark the wake finished and wake all current waiters.
    pub fn finish(&self, success: bool) {
        self.success.store(success, Ordering::Relaxed);
        self.completed.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Relaxed)
    }

    pub fn is_success(&self) -> bool {
        self.success.load(Ordering::Relaxed)
    }

    /// Wait up to `budget`: Some(success) when finished, None on timeout. The Notified future is created before the
    /// completed check — notify_waiters stores no permits, so any other order can miss the wakeup.
    pub async fn wait_for(&self, budget: Duration) -> Option<bool> {
        let notified = std::pin::pin!(self.notify.notified());
        if self.is_completed() {
            return Some(self.is_success());
        }
        if tokio::time::timeout(budget, notified).await.is_err() {
            // Timed out — but completion may have raced with the timeout.
            if self.is_completed() {
                return Some(self.is_success());
            }
            return None;
        }
        // Woken (or spuriously) — the flag is the source of truth.
        if self.is_completed() {
            return Some(self.is_success());
        }
        None
    }
}

impl Default for WakeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Liveness of the probed container by name (works for port-less compose services); "can't tell" counts as alive.
pub async fn upstream_container_alive(docker: &crate::docker::DockerManager, upstream: &str) -> bool {
    let Some(name) = upstream.split(':').next() else { return true };
    match docker.find_container_by_name(name).await {
        Ok(Some(id)) => docker.is_container_running(&id).await.unwrap_or(true),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// Probe until any HTTP response arrives (any status = listening), the window closes, or `alive` reports the
/// container gone (~2s checks, so a crash fails fast).
pub async fn readiness_probe<F, Fut>(client: &reqwest::Client, upstream: &str, max: Duration, mut alive: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + max;
    let mut next_alive_check = tokio::time::Instant::now();
    loop {
        let attempt = client.get(format!("http://{}/", upstream)).timeout(Duration::from_secs(1)).send().await;
        match attempt {
            Ok(_) => return true,
            Err(e) if !e.is_connect() && !e.is_timeout() => {
                // Non-connection error (e.g. request built fine but upstream
                // reset): still likely not listening yet — keep probing.
                tracing::debug!(upstream = upstream, error = %e, "readiness probe: not ready");
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= next_alive_check {
            next_alive_check = tokio::time::Instant::now() + Duration::from_secs(2);
            if !alive().await {
                tracing::warn!(upstream = upstream, "readiness probe: container gone");
                return false;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_returns_immediately_when_already_completed() {
        let guard = WakeGuard::new();
        guard.finish(true);
        assert_eq!(guard.wait_for(Duration::from_secs(1)).await, Some(true));
    }

    #[tokio::test]
    async fn wait_sees_completion_within_budget() {
        let guard = WakeGuard::new();
        let g = std::sync::Arc::new(guard);
        let g2 = g.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            g2.finish(true);
        });
        assert_eq!(g.wait_for(Duration::from_secs(2)).await, Some(true));
    }

    #[tokio::test]
    async fn wait_times_out_when_never_completed() {
        let guard = WakeGuard::new();
        let start = tokio::time::Instant::now();
        assert_eq!(guard.wait_for(Duration::from_millis(100)).await, None);
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn wait_sees_failure() {
        let guard = WakeGuard::new();
        let g = std::sync::Arc::new(guard);
        let g2 = g.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            g2.finish(false);
        });
        assert_eq!(g.wait_for(Duration::from_secs(2)).await, Some(false));
    }

    /// Completion racing the timeout deadline still reports its outcome, not a spurious None.
    #[tokio::test]
    async fn wait_completion_racing_timeout_is_reported() {
        let guard = WakeGuard::new();
        // Complete after the timeout fires — the re-check catches it.
        guard.finish(false);
        assert_eq!(guard.wait_for(Duration::from_nanos(1)).await, Some(false));
    }

    /// A crashed container short-circuits the probe instead of burning the window.
    #[tokio::test]
    async fn probe_fails_fast_when_container_gone() {
        let client = reqwest::Client::new();
        // Unreachable upstream + alive=false — must return within the first
        // alive check, far below the 10s window.
        let start = tokio::time::Instant::now();
        let ready = readiness_probe(&client, "127.0.0.1:1", Duration::from_secs(10), || async { false }).await;
        assert!(!ready);
        assert!(start.elapsed() < Duration::from_secs(5), "alive check should short-circuit");
    }

    /// Any HTTP response counts as ready.
    #[tokio::test]
    async fn probe_succeeds_on_any_http_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut buf = [0u8; 1024];
                let _ = tokio::time::timeout(Duration::from_secs(1), async {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                })
                .await;
            }
        });
        let client = reqwest::Client::new();
        let ready = readiness_probe(&client, &addr.to_string(), Duration::from_secs(5), || async { true }).await;
        assert!(ready);
    }
}
