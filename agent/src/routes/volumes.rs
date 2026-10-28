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

/// Relative binds (`projects/…`) must be mounted by HOST path — resolve against
/// this node's host projects dir. Other names pass through.
fn resolve_mount(state: &AgentState, name: &str) -> String {
    match name.strip_prefix("projects/") {
        Some(rest) => match state.docker.projects_host_dir() {
            Some(host) => format!("{host}/{rest}"),
            None => name.to_string(),
        },
        None => name.to_string(),
    }
}

/// POST /volumes/export?kind=&name=
pub async fn export_volume(State(state): State<AgentState>, Query(req): Query<TransferRequest>) -> impl IntoResponse {
    let Some(kind) = TransferKind::parse(&req.kind, &req.name) else {
        return error_response(StatusCode::BAD_REQUEST, format!("unknown transfer kind '{}'", req.kind));
    };
    if matches!(kind, TransferKind::Image(_)) {
        return error_response(StatusCode::BAD_REQUEST, "image transfers use /images/export".to_string());
    }
    let kind = match kind {
        TransferKind::Volume(v) => TransferKind::Volume(resolve_mount(&state, &v)),
        other => other,
    };
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
    let kind = match kind {
        TransferKind::Volume(v) => TransferKind::Volume(resolve_mount(&state, &v)),
        other => other,
    };
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

/// GET /containers/anonymous-volumes?container_id={id}
#[derive(serde::Deserialize)]
pub struct AnonymousVolumesQuery {
    pub container_id: String,
}

pub async fn anonymous_volumes(
    State(state): State<AgentState>,
    Query(q): Query<AnonymousVolumesQuery>,
) -> impl IntoResponse {
    match state.docker.anonymous_volume_names(&q.container_id).await {
        Ok(volumes) => (StatusCode::OK, Json(serde_json::json!({ "volumes": volumes }))).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("inspect failed: {e}")),
    }
}

/// GET /volumes/usage?name={volume|projects/…}
pub async fn volume_usage(
    State(state): State<AgentState>,
    Query(req): Query<litebin_common::agent_api::TransferRequest>,
) -> impl IntoResponse {
    if req.name.starts_with('/') || req.name.split('/').any(|c| c == "..") {
        return error_response(StatusCode::BAD_REQUEST, "invalid path".to_string());
    }
    let mount = resolve_mount(&state, &req.name);
    match state.docker.volume_usage_bytes(&mount).await {
        Ok(bytes) => (StatusCode::OK, Json(serde_json::json!({ "bytes": bytes }))).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("usage failed: {e}")),
    }
}

/// Body chunks are infallible for the import consumer — a client disconnect
/// mid-upload surfaces as a truncated tar that Docker's extraction rejects.
fn strip_body_error(chunk: Result<axum::body::Bytes, axum::Error>) -> axum::body::Bytes {
    chunk.unwrap_or_default()
}
