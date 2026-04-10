use std::collections::HashSet;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::{debug, warn};

use litebin_common::heartbeat::{self, FLUSH_INTERVAL_SECS};

use crate::AgentState;

type HmacSha256 = Hmac<Sha256>;

/// Background task that tails agent-caddy container logs via Docker,
/// collects unique hosts from access logs,
/// and periodically reports them to the orchestrator.
pub async fn run_activity_reporter(state: AgentState) {
    let caddy_container = std::env::var("AGENT_CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-agent-caddy".into());

    heartbeat::run_docker_log_tailer(
        state.docker.as_ref().clone(),
        caddy_container,
        FLUSH_INTERVAL_SECS,
        move |hosts| {
            let state = state.clone();
            async move {
                report_hosts_to_master(&state, hosts).await;
            }
        },
    )
    .await;
}

/// Fire-and-forget POST of active hosts to the orchestrator.
/// Uses HMAC signing (same pattern as wake report).
async fn report_hosts_to_master(state: &AgentState, hosts: HashSet<String>) {
    let reg = match state.registration.read().unwrap().clone() {
        Some(r) => r,
        None => return,
    };

    let heartbeat_url = reg.heartbeat_url.trim_end_matches("/internal/heartbeat");
    let url = format!("{}/internal/heartbeat", heartbeat_url);

    let node_id = reg.node_id.clone();
    let secret = reg.secret.clone();
    let timestamp = chrono::Utc::now().timestamp();
    let message = format!("{}\n{}", timestamp, node_id);

    let signature = match compute_hmac(&secret, &message) {
        Some(s) => s,
        None => return,
    };

    let body = serde_json::json!({
        "hosts": hosts.into_iter().collect::<Vec<_>>()
    });

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "activity reporter: failed to build HTTP client");
            return;
        }
    };

    match client
        .post(&url)
        .header("X-Agent-Id", &node_id)
        .header("X-Agent-Timestamp", timestamp.to_string())
        .header("X-Agent-Signature", &signature)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!("activity reporter: heartbeat sent to orchestrator");
        }
        Ok(resp) => {
            warn!(
                status = %resp.status(),
                "activity reporter: orchestrator returned non-success"
            );
        }
        Err(e) => {
            debug!(error = %e, "activity reporter: failed to reach orchestrator (fire-and-forget)");
        }
    }
}

fn compute_hmac(secret: &str, message: &str) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}
