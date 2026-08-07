use axum::{Json, extract::State, http::StatusCode};
use serde_json::Value;

use crate::AgentState;

/// POST /caddy/sync
/// Receives a full Caddy JSON config from the orchestrator, persists it to disk,
/// and pushes it to the local Caddy sidecar's Admin API.
pub async fn sync_caddy(State(state): State<AgentState>, Json(config): Json<Value>) -> StatusCode {
    let caddy = match state.caddy.as_ref() {
        Some(c) => c,
        None => {
            tracing::warn!("caddy sync requested but no caddy client configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    // Persist the orchestrator-pushed base (raw) to state + file, so
    // rebuild_local_caddy can use it as a base later.
    {
        let mut guard = state.last_caddy_config.write().unwrap();
        *guard = Some(config.clone());
    }
    crate::save_caddy_config_to_file(&config);

    // Push an ENRICHED copy: re-add the /__l8b_upload route, the agent cert
    // (SNI=agent), and the agent's own on-demand ask endpoint. The orchestrator's
    // config omits/clobbers these, so every sync must re-apply them — otherwise
    // direct uploads and agent-local TLS issuance break until the next local rebuild.
    let mut enriched = config.clone();
    let upload_upstream = format!("litebin-agent:{}", state.config.upload_port);
    crate::routes::waker::enrich_agent_config(
        &mut enriched,
        &state.config.cert_pem,
        &state.config.key_pem,
        &upload_upstream,
    );

    let url = format!("{}/load", caddy.admin_url());
    match caddy.post_json(&url, &enriched).await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("agent caddy config loaded and persisted");
            StatusCode::OK
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, "agent caddy /load failed: {}", body);
            StatusCode::BAD_GATEWAY
        }
        Err(e) => {
            tracing::warn!(error = %e, "agent caddy /load request failed");
            StatusCode::BAD_GATEWAY
        }
    }
}
