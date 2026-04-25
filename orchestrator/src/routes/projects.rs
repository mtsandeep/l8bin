use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::auth::backend::PasswordBackend;
use crate::db::models::Project;
use crate::AppState;

use super::stats::{ServiceInfo, ServiceVolumeInfo};
use litebin_common::types::{VolumeMount, scope_volume_source};

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

// ── Public Stats (service-level data for the public service) ──────────────────
// Reuses ServiceInfo from stats.rs — public_stats is just one service's info.

/// Project response for the API — project metadata + public_stats.
/// The internal `Project` struct (with all DB columns) is used by backend code;
/// this struct is the API-facing shape.
#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    pub id: String,
    pub user_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_id: Option<String>,
    pub status: String,
    pub last_active_at: Option<i64>,
    pub auto_stop_enabled: bool,
    pub auto_stop_timeout_mins: i64,
    pub auto_start_enabled: bool,
    pub custom_domain: Option<String>,
    pub service_count: Option<i64>,
    pub service_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub public_stats: Option<ServiceInfo>,
}

/// Build PublicStats for a single-service project (no project_services row).
fn public_stats_from_project(project: &Project) -> Option<ServiceInfo> {
    let image = project.image.as_deref()?.to_string();
    if image.is_empty() {
        return None;
    }

    // Parse volumes JSON and convert to ServiceVolumeInfo
    let volumes: Vec<ServiceVolumeInfo> = match &project.volumes {
        Some(json) => serde_json::from_str::<Vec<VolumeMount>>(json)
            .unwrap_or_default()
            .into_iter()
            .map(|v| ServiceVolumeInfo {
                volume_name: v.name.map(|name| scope_volume_source(&name, &project.id)),
                container_path: v.path,
            })
            .collect(),
        None => vec![],
    };

    Some(ServiceInfo {
        service_name: "web".to_string(),
        image,
        port: project.internal_port,
        mapped_port: project.mapped_port,
        is_public: true,
        status: project.status.clone(),
        container_id: project.container_id.clone(),
        cmd: project.cmd.clone(),
        cpu_percent: None,
        memory_usage: None,
        memory_limit_mb: project.memory_limit_mb,
        cpu_limit: project.cpu_limit,
        disk_gb: None,
        volumes,
    })
}

/// Build ProjectResponse from a Project row.
async fn to_project_response(
    project: &Project,
    db: &sqlx::SqlitePool,
) -> ProjectResponse {
    let public_stats = if project.service_count.unwrap_or(1) > 1 {
        // Multi-service: look up the public service from project_services
        let row: Option<(String, String, Option<i64>, Option<i64>, bool, String, Option<String>, Option<String>, Option<i64>, Option<f64>)> = sqlx::query_as(
            "SELECT service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ? AND is_public = 1 LIMIT 1"
        )
        .bind(&project.id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

        match row {
            Some((service_name, image, port, mapped_port, is_public, status, container_id, _cmd, memory_limit_mb, cpu_limit)) => {
                // Load volumes for this service from project_volumes
                let vol_rows: Vec<(Option<String>, String)> = sqlx::query_as(
                    "SELECT volume_name, container_path FROM project_volumes WHERE project_id = ? AND service_name = ?"
                )
                .bind(&project.id)
                .bind(&service_name)
                .fetch_all(db)
                .await
                .unwrap_or_default();

                let volumes: Vec<ServiceVolumeInfo> = vol_rows
                    .into_iter()
                    .map(|(volume_name, container_path)| ServiceVolumeInfo { volume_name, container_path })
                    .collect();

                Some(ServiceInfo {
                    service_name,
                    image,
                    port,
                    mapped_port,
                    is_public,
                    status,
                    container_id,
                    cmd: None, // multi-service uses compose for commands
                    cpu_percent: None,
                    memory_usage: None,
                    memory_limit_mb,
                    cpu_limit,
                    disk_gb: None,
                    volumes,
                })
            }
            None => None,
        }
    } else {
        // Single-service: build from project row
        public_stats_from_project(project)
    };

    ProjectResponse {
        id: project.id.clone(),
        user_id: project.user_id.clone(),
        name: project.name.clone(),
        description: project.description.clone(),
        node_id: project.node_id.clone(),
        status: project.status.clone(),
        last_active_at: project.last_active_at,
        auto_stop_enabled: project.auto_stop_enabled,
        auto_stop_timeout_mins: project.auto_stop_timeout_mins,
        auto_start_enabled: project.auto_start_enabled,
        custom_domain: project.custom_domain.clone(),
        service_count: project.service_count,
        service_summary: project.service_summary.clone(),
        created_at: project.created_at,
        updated_at: project.updated_at,
        public_stats,
    }
}

pub async fn create_project(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<ProjectResponse>), (StatusCode, Json<serde_json::Value>)> {
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

    // Reject project IDs that conflict with existing alias routes
    let alias_conflict: Option<String> = sqlx::query_scalar(
        "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1"
    )
    .bind(&payload.id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(pid) = alias_conflict {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": format!("project ID '{}' is already used as an alias for project '{}'", payload.id, pid)})),
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
            let response = to_project_response(&project, &state.db).await;
            Ok((StatusCode::CREATED, Json(response)))
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
) -> Result<Json<ProjectResponse>, (StatusCode, String)> {
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
        Some(p) => {
            let response = to_project_response(&p, &state.db).await;
            Ok(Json(response))
        }
        None => Err((StatusCode::NOT_FOUND, format!("project '{id}' not found"))),
    }
}

pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectResponse>>, (StatusCode, String)> {
    let projects = sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY updated_at DESC")
        .fetch_all(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {e}"),
            )
        })?;

    let mut responses = Vec::with_capacity(projects.len());
    for project in &projects {
        let response = to_project_response(project, &state.db).await;
        responses.push(response);
    }

    Ok(Json(responses))
}

// ── Custom Routes ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRouteRequest {
    pub route_type: String,       // "path" or "alias"
    pub path: Option<String>,
    pub subdomain: Option<String>,
    pub upstream: String,
    pub priority: Option<i64>,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct ProjectRouteResponse {
    pub id: String,
    pub project_id: String,
    pub route_type: String,
    pub path: Option<String>,
    pub subdomain: Option<String>,
    pub upstream: String,
    pub priority: i64,
    pub created_at: i64,
}

/// GET /projects/:id/routes — List custom routes for a project.
pub async fn list_routes(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"}))).into_response();
    }

    let routes: Vec<ProjectRouteResponse> = match sqlx::query_as(
        "SELECT id, project_id, route_type, path, subdomain, upstream, priority, created_at FROM project_routes WHERE project_id = ? ORDER BY priority, created_at"
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")}))).into_response();
        }
    };

    (StatusCode::OK, Json(routes)).into_response()
}

/// POST /projects/:id/routes — Create a custom route.
pub async fn create_route(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<CreateRouteRequest>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"}))).into_response();
    }

    if payload.route_type != "path" && payload.route_type != "alias" {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "route_type must be 'path' or 'alias'"}))).into_response();
    }

    if payload.route_type == "path" && payload.path.is_none() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "path is required for path-based routes"}))).into_response();
    }

    if payload.route_type == "alias" && payload.subdomain.is_none() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "subdomain is required for alias routes"}))).into_response();
    }

    // Reject alias values that conflict with existing project IDs or other aliases
    if payload.route_type == "alias" {
        let alias = payload.subdomain.as_deref().unwrap_or("");
        if !alias.is_empty() {
            // Check against project IDs
            let conflicts = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM projects WHERE id = ?"
            )
            .bind(alias)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

            if conflicts > 0 {
                return (StatusCode::CONFLICT, Json(serde_json::json!({"error": format!("alias '{}' conflicts with an existing project ID", alias)}))).into_response();
            }

            // Check against existing aliases on other projects
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? AND project_id != ? LIMIT 1"
            )
            .bind(alias)
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some(other_pid) = existing {
                return (StatusCode::CONFLICT, Json(serde_json::json!({"error": format!("alias '{}' is already used by project '{}'", alias, other_pid)}))).into_response();
            }
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let priority = payload.priority.unwrap_or(100);

    if let Err(e) = sqlx::query(
        "INSERT INTO project_routes (id, project_id, route_type, path, subdomain, upstream, priority, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&project_id)
    .bind(&payload.route_type)
    .bind(&payload.path)
    .bind(&payload.subdomain)
    .bind(&payload.upstream)
    .bind(priority)
    .bind(now)
    .execute(&state.db)
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")}))).into_response();
    }

    // Trigger Caddy resync
    let _ = state.route_sync_tx.send(());

    (StatusCode::OK, Json(ProjectRouteResponse {
        id,
        project_id,
        route_type: payload.route_type,
        path: payload.path,
        subdomain: payload.subdomain,
        upstream: payload.upstream,
        priority,
        created_at: now,
    })).into_response()
}

/// DELETE /projects/:id/routes/:route_id — Delete a custom route.
pub async fn delete_route(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path((project_id, route_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"}))).into_response();
    }

    let result = sqlx::query("DELETE FROM project_routes WHERE id = ? AND project_id = ?")
        .bind(&route_id)
        .bind(&project_id)
        .execute(&state.db)
        .await;

    match result {
        Ok(row) if row.rows_affected() > 0 => {
            // Trigger Caddy resync
            let _ = state.route_sync_tx.send(());
            (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Route not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")}))).into_response(),
    }
}
