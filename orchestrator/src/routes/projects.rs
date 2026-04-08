use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use axum_login::AuthSession;
use serde::Deserialize;

use crate::auth::backend::PasswordBackend;
use crate::db::models::Project;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

pub async fn create_project(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), (StatusCode, Json<serde_json::Value>)> {
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Authentication required"})),
            ));
        }
    };

    // Validate project ID (DNS-safe label)
    if !crate::validation::is_valid_project_id(&payload.id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens (no leading/trailing hyphens)"})),
        ));
    }

    // Reserve the dashboard subdomain
    if payload.id == state.config.dashboard_subdomain {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "This ID is reserved"})),
        ));
    }

    // Reserve the poke subdomain
    if payload.id == state.config.poke_subdomain {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "This ID is reserved"})),
        ));
    }

    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        "INSERT INTO projects (id, user_id, name, description, status, created_at, updated_at) VALUES (?, ?, ?, ?, 'unconfigured', ?, ?)",
    )
    .bind(&payload.id)
    .bind(&user_id)
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
                .bind(&payload.id)
                .fetch_one(&state.db)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("{e}")}))))?;
            Ok((StatusCode::CREATED, Json(project)))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("UNIQUE constraint") {
                Err((StatusCode::CONFLICT, Json(serde_json::json!({"error": "Project already exists"}))))
            } else {
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": msg}))))
            }
        }
    }
}

pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Project>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {e}"),
            )
        })?;

    match project {
        Some(p) => Ok(Json(p)),
        None => Err((StatusCode::NOT_FOUND, format!("project '{id}' not found"))),
    }
}

pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, (StatusCode, String)> {
    let projects = sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY updated_at DESC")
        .fetch_all(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {e}"),
            )
        })?;

    Ok(Json(projects))
}
