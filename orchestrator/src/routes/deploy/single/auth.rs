use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde_json::json;

use crate::AppState;
use crate::auth::backend::PasswordBackend;

use super::types::DeployRequest;

/// Authenticate via session or deploy token.
pub(super) async fn authenticate(
    auth_session: &AuthSession<PasswordBackend>,
    state: &AppState,
    headers: &HeaderMap,
    project_id: &str,
) -> Result<String, axum::response::Response> {
    match &auth_session.user {
        Some(u) => Ok(u.id.clone()),
        None => match crate::auth::extract_deploy_token(state, headers, project_id).await {
            Some(uid) => Ok(uid),
            None => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
            )
                .into_response()),
        },
    }
}

/// Validate project ID: reserved subdomains, DNS-safe label, alias conflicts.
pub(super) async fn validate_project_id(state: &AppState, payload: &DeployRequest) -> Option<axum::response::Response> {
    if payload.project_id == state.platform.dashboard_subdomain() {
        return Some((StatusCode::BAD_REQUEST, Json(json!({"error": "This ID is reserved"}))).into_response());
    }
    if payload.project_id == state.platform.poke_subdomain() {
        return Some((StatusCode::BAD_REQUEST, Json(json!({"error": "This ID is reserved"}))).into_response());
    }
    if !crate::validation::is_valid_project_id(&payload.project_id) {
        return Some((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens (no leading/trailing hyphens)"})),
        ).into_response());
    }

    // Reject project IDs that conflict with existing alias routes
    let alias_conflict: Option<String> = sqlx::query_scalar(
        "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1",
    )
    .bind(&payload.project_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(pid) = alias_conflict {
        return Some((
            StatusCode::CONFLICT,
            Json(json!({"error": format!("project ID '{}' is already used as an alias for project '{}'", payload.project_id, pid)})),
        ).into_response());
    }

    None
}
