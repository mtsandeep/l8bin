use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use serde_json::json;

use crate::auth::backend::PasswordBackend;
use crate::AppState;

/// DELETE /projects/:id/volumes/:name — Remove a specific volume.
pub async fn delete_volume(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path((project_id, name)): Path<(String, String)>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }

    let scoped = litebin_common::types::scope_volume_source(&name, &project_id);

    if let Err(e) = state.docker.remove_volume_by_name(&scoped).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to remove volume: {e}")})),
        ).into_response();
    }

    tracing::info!(project = %project_id, volume = %scoped, "volume deleted");
    (StatusCode::OK, Json(json!({"deleted": scoped}))).into_response()
}

/// DELETE /projects/:id/volumes — Remove all volumes for a project.
pub async fn delete_all_volumes(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }

    // Collect all scoped volume names for the project
    let volumes: Vec<String> = sqlx::query_as::<_, (String,)>(
        "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(name,)| name)
    .collect();

    let mut deleted: Vec<String> = Vec::new();
    for vol_name in &volumes {
        if state.docker.remove_volume_by_name(vol_name).await.is_ok() {
            deleted.push(vol_name.clone());
        }
    }

    tracing::info!(project = %project_id, count = deleted.len(), "all volumes deleted");
    (StatusCode::OK, Json(json!({"deleted": deleted}))).into_response()
}
