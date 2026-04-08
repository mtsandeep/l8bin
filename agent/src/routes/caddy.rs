use axum::{extract::State, http::StatusCode, Json};
use serde_json::Value;

use crate::AgentState;

/// POST /caddy/sync
/// Receives a full Caddy JSON config from the orchestrator and pushes it to the
/// local Caddy sidecar's Admin API.
pub async fn sync_caddy(
    State(state): State<AgentState>,
    Json(config): Json<Value>,
) -> StatusCode {
    let caddy = match state.caddy.as_ref() {
        Some(c) => c,
        None => {
            tracing::warn!("caddy sync requested but no caddy client configured");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };

    let url = format!("{}/load", caddy.admin_url());
    match caddy.post_json(&url, &config).await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("agent caddy config loaded");
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
