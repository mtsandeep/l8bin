use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

use crate::AgentState;

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// POST /volumes/export — not yet implemented
pub async fn export_volume(State(_state): State<AgentState>) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse { error: "volume export not yet implemented".to_string() }),
    )
}

/// POST /volumes/import — not yet implemented
pub async fn import_volume(State(_state): State<AgentState>) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse { error: "volume import not yet implemented".to_string() }),
    )
}
