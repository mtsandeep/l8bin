//! Orchestrator-facing read/write of this node's project `.env` files.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use litebin_common::agent_api::{EnvFileResponse, EnvWriteRequest};

use super::containers::env::{env_has_changed, projects_dir, read_env_raw};
use crate::AgentState;

#[derive(Deserialize)]
pub struct EnvQuery {
    pub project_id: String,
}

/// GET /internal/env?project_id=… — raw `.env` content + pending-changes flag.
pub async fn get_env(State(_state): State<AgentState>, Query(q): Query<EnvQuery>) -> axum::response::Response {
    let content = read_env_raw(&q.project_id);
    let has_pending_changes = env_has_changed(&q.project_id);
    (StatusCode::OK, Json(EnvFileResponse { content, has_pending_changes })).into_response()
}

/// POST /internal/env — replace the project's `.env` (0600).
pub async fn write_env(State(_state): State<AgentState>, Json(req): Json<EnvWriteRequest>) -> axum::response::Response {
    if req.project_id.is_empty() || req.project_id.contains('/') || req.project_id.contains('\\') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid project id"}))).into_response();
    }
    if req.content.len() > MAX_ENV_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("env file exceeds {} bytes", MAX_ENV_BYTES)})),
        )
            .into_response();
    }

    let project_dir = projects_dir().join(&req.project_id);
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to create project directory: {e}")})),
        )
            .into_response();
    }

    let env_path = project_dir.join(".env");
    if let Err(e) = std::fs::write(&env_path, &req.content) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("failed to write .env: {e}")})))
            .into_response();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(project = %req.project_id, bytes = req.content.len(), "runtime .env updated via orchestrator");
    (StatusCode::OK, Json(json!({"status": "updated"}))).into_response()
}

const MAX_ENV_BYTES: usize = 64 * 1024;
