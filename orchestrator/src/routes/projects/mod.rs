mod custom_routes;
mod response;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use axum_login::AuthSession;
use serde::Deserialize;

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::db::models::Project;

pub use custom_routes::*;
use response::to_project_response;
pub use response::*;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateProjectRequest {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub is_background: bool,
}

#[utoipa::path(
    post,
    path = "/projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 201, body = ProjectResponse),
        (status = 401),
        (status = 400),
        (status = 409),
        (status = 500),
    ),
    tag = "projects",
    security(("session_auth" = []))
)]
pub async fn create_project(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<ProjectResponse>), (StatusCode, Json<serde_json::Value>)> {
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            return Err((StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"}))));
        }
    };

    // Validate project ID (DNS-safe label)
    if !crate::validation::is_valid_project_id(&payload.id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens (no leading/trailing hyphens)"}),
            ),
        ));
    }

    // Reserve the dashboard subdomain
    if payload.id == state.platform.dashboard_subdomain() {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "This ID is reserved"}))));
    }

    // Reserve the poke subdomain
    if payload.id == state.platform.poke_subdomain() {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "This ID is reserved"}))));
    }

    // Reject project IDs that conflict with existing alias routes
    let alias_conflict: Option<String> = sqlx::query_scalar(
        "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1",
    )
    .bind(&payload.id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(pid) = alias_conflict {
        return Err((
            StatusCode::CONFLICT,
            Json(
                serde_json::json!({"error": format!("project ID '{}' is already used as an alias for project '{}'", payload.id, pid)}),
            ),
        ));
    }

    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        "INSERT INTO projects (id, user_id, name, description, is_background, status, auto_stop_enabled, auto_start_enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
    )
    .bind(&payload.id)
    .bind(&user_id)
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(payload.is_background)
    .bind(!payload.is_background)
    .bind(!payload.is_background)
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
            let response = to_project_response(&project, &state.db).await;
            Ok((StatusCode::CREATED, Json(response)))
        }
        Err(e) => {
            if crate::validation::is_unique_constraint(&e) {
                Err((StatusCode::CONFLICT, Json(serde_json::json!({"error": "Project already exists"}))))
            } else {
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))
            }
        }
    }
}

#[utoipa::path(
    get,
    path = "/projects/{id}",
    params(
        ("id" = String, Path)
    ),
    responses(
        (status = 200, body = ProjectResponse),
        (status = 404),
        (status = 500),
    ),
    tag = "projects",
    security(("session_auth" = []))
)]
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?;

    match project {
        Some(p) => {
            let response = to_project_response(&p, &state.db).await;
            Ok(Json(response))
        }
        None => Err((StatusCode::NOT_FOUND, format!("project '{id}' not found"))),
    }
}

#[utoipa::path(
    get,
    path = "/projects",
    responses(
        (status = 200, body = Vec<ProjectResponse>),
        (status = 500),
    ),
    tag = "projects",
    security(("session_auth" = []))
)]
pub async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<ProjectResponse>>, (StatusCode, String)> {
    let projects = sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY updated_at DESC")
        .fetch_all(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?;

    let mut responses = Vec::with_capacity(projects.len());
    for project in &projects {
        let response = to_project_response(project, &state.db).await;
        responses.push(response);
    }

    Ok(Json(responses))
}
