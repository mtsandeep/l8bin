use axum::response::IntoResponse;
use axum::http::{header, StatusCode};
use axum::{extract::State, Json};
use utoipa::OpenApi;

use crate::openapi::ApiDoc;
use crate::AppState;

/// GET /openapi.json — Serve the OpenAPI 3.1 spec.
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// GET /llms.txt — Serve the llms.txt file for AI agents.
pub async fn llms_txt(State(_state): State<AppState>) -> impl IntoResponse {
    // Embedded at compile time — always in sync with this binary version.
    let content = include_str!("../../../docs/llms.txt");
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], content.to_string())
}
