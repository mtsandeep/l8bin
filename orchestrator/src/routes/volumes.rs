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

/// DELETE /projects/:id/volumes/:name — Remove a specific volume data directory.
pub async fn delete_volume(
    auth_session: AuthSession<PasswordBackend>,
    State(_state): State<AppState>,
    Path((project_id, name)): Path<(String, String)>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }

    let path = std::path::PathBuf::from("projects")
        .join(&project_id)
        .join("data")
        .join(&name);

    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "Volume directory not found"}))).into_response();
    }

    if let Err(e) = std::fs::remove_dir_all(&path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to remove volume: {e}")})),
        ).into_response();
    }

    tracing::info!(project = %project_id, volume = %name, "volume data deleted");
    (StatusCode::OK, Json(json!({"deleted": path.display().to_string()}))).into_response()
}

/// DELETE /projects/:id/volumes — Remove all volume data directories for a project.
pub async fn delete_all_volumes(
    auth_session: AuthSession<PasswordBackend>,
    State(_state): State<AppState>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }

    let data_dir = std::path::PathBuf::from("projects")
        .join(&project_id)
        .join("data");

    if !data_dir.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "No data directory found"}))).into_response();
    }

    let mut deleted: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                    tracing::warn!(path = %entry.path().display(), error = %e, "failed to remove volume dir");
                } else {
                    deleted.push(entry.path().display().to_string());
                }
            }
        }
    }

    tracing::info!(project = %project_id, count = deleted.len(), "all volume data deleted");
    (StatusCode::OK, Json(json!({"deleted": deleted}))).into_response()
}
