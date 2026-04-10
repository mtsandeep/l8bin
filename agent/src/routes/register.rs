use axum::{extract::State, http::StatusCode, Json};

use crate::config::AgentRegistration;

#[derive(serde::Deserialize)]
pub struct RegisterRequest {
    pub node_id: String,
    pub secret: String,
    pub domain: String,
    pub wake_report_url: String,
    pub heartbeat_url: String,
}

/// POST /internal/register — called by orchestrator over mTLS to push config.
pub async fn register(
    State(state): State<crate::AgentState>,
    Json(req): Json<RegisterRequest>,
) -> StatusCode {
    let reg = AgentRegistration {
        node_id: req.node_id,
        secret: req.secret,
        domain: req.domain,
        wake_report_url: req.wake_report_url,
        heartbeat_url: req.heartbeat_url,
    };

    tracing::info!(
        node_id = %reg.node_id,
        domain = %reg.domain,
        "received registration from orchestrator"
    );

    // Persist to file for restarts
    if let Err(e) = crate::save_registration_to_file(&reg) {
        tracing::error!(error = %e, "failed to persist registration");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    // Update in-memory state
    {
        let mut guard = state.registration.write().unwrap();
        *guard = Some(reg);
    }

    StatusCode::OK
}
