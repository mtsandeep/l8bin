mod auth;
mod background;
mod core;
mod types;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde_json::json;

use crate::AppState;
use crate::auth::backend::PasswordBackend;

use auth::{authenticate, validate_project_id};
use core::execute_deploy;
pub use types::{DeployRequest, DeployResponse};

#[utoipa::path(
    post,
    path = "/deploy",
    request_body = DeployRequest,
    responses(
        (status = 200, description = "Deployment started", body = DeployResponse),
        (status = 401, description = "Authentication required"),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Project already exists"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy",
    security(("session_auth" = []), ("bearer_token" = [])),
)]
/// POST /deploy — Create a new project deployment (fails with 409 if project already exists).
pub async fn deploy_create(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DeployRequest>,
) -> impl IntoResponse {
    let user_id = match authenticate(&auth_session, &state, &headers, &payload.project_id).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    // Validate
    if let Some(resp) = validate_project_id(&state, &payload).await {
        return resp;
    }

    // Check project doesn't already exist
    let exists: i64 = match sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(count) => count,
        Err(e) => {
            tracing::error!(project_id = %payload.project_id, error = %e, "deploy: failed to check project existence");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "database error"}))).into_response();
        }
    };
    if exists > 0 {
        return (StatusCode::CONFLICT, Json(json!({"error": "Project already exists"}))).into_response();
    }

    execute_deploy(state, user_id, payload, false).await
}

#[utoipa::path(
    put,
    path = "/deploy",
    request_body = DeployRequest,
    responses(
        (status = 200, description = "Deployment started", body = DeployResponse),
        (status = 401, description = "Authentication required"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy",
    security(("session_auth" = []), ("bearer_token" = [])),
)]
/// PUT /deploy — Redeploy an existing project (upserts, creating if missing).
pub async fn deploy_update(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DeployRequest>,
) -> impl IntoResponse {
    let user_id = match authenticate(&auth_session, &state, &headers, &payload.project_id).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    // Validate
    if let Some(resp) = validate_project_id(&state, &payload).await {
        return resp;
    }

    execute_deploy(state, user_id, payload, true).await
}
