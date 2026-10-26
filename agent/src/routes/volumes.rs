//! Transfer endpoints for project migration: stream volumes, project
//! directories, and images as tars between nodes.

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use futures_util::StreamExt;
use litebin_common::agent_api::{ErrorResponse, TransferRequest};
use litebin_common::docker::TransferKind;

use crate::AgentState;

/// POST /volumes/export?kind=&name=
pub async fn export_volume(State(state): State<AgentState>, Query(req): Query<TransferRequest>) -> impl IntoResponse {
    let Some(kind) = TransferKind::parse(&req.kind, &req.name) else {
        return error_response(StatusCode::BAD_REQUEST, format!("unknown transfer kind '{}'", req.kind));
    };
    if matches!(kind, TransferKind::Image(_)) {
        return error_response(StatusCode::BAD_REQUEST, "image transfers use /images/export".to_string());
    }
    match state.docker.transfer_export(&kind, state.docker.projects_host_dir()).await {
        Ok(stream) => {
            let body = Body::from_stream(stream);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/x-tar")], body).into_response()
        }
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("export failed: {e}")),
    }
}

/// POST /volumes/import?kind=&name=  (body: tar stream)
pub async fn import_volume(
    State(state): State<AgentState>,
    Query(req): Query<TransferRequest>,
    body: Body,
) -> impl IntoResponse {
    let Some(kind) = TransferKind::parse(&req.kind, &req.name) else {
        return error_response(StatusCode::BAD_REQUEST, format!("unknown transfer kind '{}'", req.kind));
    };
    if matches!(kind, TransferKind::Image(_)) {
        return error_response(StatusCode::BAD_REQUEST, "image transfers use /images/load".to_string());
    }
    let tar = body.into_data_stream().map(strip_body_error);
    match state.docker.transfer_import(&kind, state.docker.projects_host_dir(), tar).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"imported": req.name}))).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("import failed: {e}")),
    }
}

/// POST /images/export?name=  (docker-save tar stream)
pub async fn export_image(State(state): State<AgentState>, Query(req): Query<TransferRequest>) -> impl IntoResponse {
    let kind = TransferKind::Image(req.name.clone());
    match state.docker.transfer_export(&kind, state.docker.projects_host_dir()).await {
        Ok(stream) => {
            let body = Body::from_stream(stream);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/x-tar")], body).into_response()
        }
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("export failed: {e}")),
    }
}

fn error_response(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(ErrorResponse { error: message })).into_response()
}

/// Body chunks are infallible for the import consumer — a client disconnect
/// mid-upload surfaces as a truncated tar that Docker's extraction rejects.
fn strip_body_error(chunk: Result<axum::body::Bytes, axum::Error>) -> axum::body::Bytes {
    chunk.unwrap_or_default()
}
