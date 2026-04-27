use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use serde::Deserialize;

use crate::auth::backend::PasswordBackend;
use crate::db::models::Project;
use crate::AppState;

#[derive(Deserialize)]
pub struct UpdateSettingsRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub custom_domain: Option<String>,
    pub auto_stop_enabled: Option<bool>,
    pub auto_stop_timeout_mins: Option<i64>,
    pub auto_start_enabled: Option<bool>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
}

pub async fn update_project_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateSettingsRequest>,
) -> Result<Json<Project>, (StatusCode, String)> {
    // Validate auto_stop_timeout_mins >= 1
    if let Some(mins) = payload.auto_stop_timeout_mins {
        if mins < 1 {
            return Err((
                StatusCode::BAD_REQUEST,
                "auto_stop_timeout_mins must be at least 1".to_string(),
            ));
        }
    }

    // Normalize and validate custom_domain if provided
    // Empty string → clear (set to NULL); non-empty → validate format & uniqueness
    let resolved_domain: Option<Option<String>> = if let Some(ref d) = payload.custom_domain {
        let trimmed = d.trim().to_lowercase();
        if trimmed.is_empty() {
            // Empty string means "clear the domain"
            Some(None)
        } else {
            // Strip www. prefix — canonical form stored without www
            let trimmed = trimmed.strip_prefix("www.").unwrap_or(&trimmed).to_string();
            if !trimmed.contains('.') || trimmed.starts_with('.') || trimmed.ends_with('.') {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "invalid domain format".to_string(),
                ));
            }
            // Uniqueness check
            let conflict: Option<String> = sqlx::query_scalar(
                "SELECT id FROM projects WHERE custom_domain = ? AND id != ?",
            )
            .bind(&trimmed)
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?
            .flatten();

            if let Some(conflict_id) = conflict {
                return Err((
                    StatusCode::CONFLICT,
                    format!("domain already in use by project '{}'", conflict_id),
                ));
            }
            Some(Some(trimmed))
        }
    } else {
        None // not provided in request
    };

    // Check project exists
    let existing = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {e}"),
            )
        })?;

    if existing.is_none() {
        return Err((StatusCode::NOT_FOUND, format!("project '{}' not found", id)));
    }
    let old_domain = existing.as_ref().unwrap().custom_domain.clone();

    let now = chrono::Utc::now().timestamp();

    // Build dynamic UPDATE — only set fields present in the request
    let mut set_clauses: Vec<&str> = vec!["updated_at = ?"];
    let mut has_name = false;
    let mut has_description = false;
    let mut has_custom_domain = false;
    let mut has_auto_stop_enabled = false;
    let mut has_auto_stop_timeout_mins = false;
    let mut has_auto_start_enabled = false;
    let mut has_cmd = false;
    let mut has_memory_limit_mb = false;
    let mut has_cpu_limit = false;

    if payload.name.is_some() {
        set_clauses.push("name = ?");
        has_name = true;
    }
    if payload.description.is_some() {
        set_clauses.push("description = ?");
        has_description = true;
    }
    if resolved_domain.is_some() {
        set_clauses.push("custom_domain = ?");
        has_custom_domain = true;
    }
    if payload.auto_stop_enabled.is_some() {
        set_clauses.push("auto_stop_enabled = ?");
        has_auto_stop_enabled = true;
    }
    if payload.auto_stop_timeout_mins.is_some() {
        set_clauses.push("auto_stop_timeout_mins = ?");
        has_auto_stop_timeout_mins = true;
    }
    if payload.auto_start_enabled.is_some() {
        set_clauses.push("auto_start_enabled = ?");
        has_auto_start_enabled = true;
    }
    if payload.cmd.is_some() {
        set_clauses.push("cmd = ?");
        has_cmd = true;
    }
    if payload.memory_limit_mb.is_some() {
        set_clauses.push("memory_limit_mb = ?");
        has_memory_limit_mb = true;
    }
    if payload.cpu_limit.is_some() {
        set_clauses.push("cpu_limit = ?");
        has_cpu_limit = true;
    }

    if !has_name && !has_description && !has_custom_domain && !has_auto_stop_enabled && !has_auto_stop_timeout_mins && !has_auto_start_enabled && !has_cmd && !has_memory_limit_mb && !has_cpu_limit {
        return Ok(Json(existing.unwrap()));
    }

    let sql = format!("UPDATE projects SET {} WHERE id = ?", set_clauses.join(", "));
    let mut query = sqlx::query(&sql).bind(now);

    // Track if domain changed (for Caddy resync) before consuming resolved_domain
    let domain_changed = has_custom_domain && resolved_domain.as_ref().unwrap() != &old_domain;

    // Bind order must match set_clauses order
    if has_name {
        let v = payload.name.unwrap();
        query = query.bind(if v.trim().is_empty() { None } else { Some(v) });
    }
    if has_description {
        let v = payload.description.unwrap();
        query = query.bind(if v.trim().is_empty() { None } else { Some(v) });
    }
    if has_custom_domain {
        // resolved_domain is Some(Some(domain)) or Some(None) for clearing
        query = query.bind(resolved_domain.unwrap());
    }
    if has_auto_stop_enabled { query = query.bind(payload.auto_stop_enabled.unwrap()); }
    if has_auto_stop_timeout_mins { query = query.bind(payload.auto_stop_timeout_mins.unwrap()); }
    if has_auto_start_enabled { query = query.bind(payload.auto_start_enabled.unwrap()); }
    if has_cmd { query = query.bind(payload.cmd.as_deref().filter(|s| !s.is_empty())); }
    if has_memory_limit_mb { query = query.bind(payload.memory_limit_mb.unwrap()); }
    if has_cpu_limit { query = query.bind(payload.cpu_limit.unwrap()); }

    query.bind(&id).execute(&state.db).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("database error: {e}"),
        )
    })?;

    // For multi-service projects, also update the public service in project_services
    if (has_memory_limit_mb || has_cpu_limit) && existing.as_ref().unwrap().service_count.unwrap_or(1) > 1 {
        let mut set_svc: Vec<&str> = Vec::new();
        if has_memory_limit_mb { set_svc.push("memory_limit_mb = ?"); }
        if has_cpu_limit { set_svc.push("cpu_limit = ?"); }
        let sql = format!("UPDATE project_services SET {} WHERE project_id = ? AND is_public = 1", set_svc.join(", "));
        let mut svc_query = sqlx::query(&sql);
        if has_memory_limit_mb { svc_query = svc_query.bind(payload.memory_limit_mb.unwrap()); }
        if has_cpu_limit { svc_query = svc_query.bind(payload.cpu_limit.unwrap()); }
        svc_query = svc_query.bind(&id);
        let _ = svc_query.execute(&state.db).await;
    }

    // Resync Caddy if custom_domain changed
    if domain_changed {
        crate::routes::manage::sync_caddy(&state).await;
    }

    // Fetch and return the updated project
    let updated = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("database error: {e}"),
            )
        })?;

    // Push project meta to agent if auto_start_enabled changed and project is on a remote node
    if has_auto_start_enabled {
        if let Some(ref node_id) = updated.node_id {
            if node_id != "local" {
                crate::cloudflare_router::push_project_meta_to_agent(
                    node_id,
                    &state.db,
                    &state.node_clients,
                    &state.config,
                )
                .await;
            }
        }
    }

    Ok(Json(updated))
}

// ── Per-Service Settings ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateServiceSettingsRequest {
    pub memory_limit_mb: Option<Option<i64>>,
    pub cpu_limit: Option<Option<f64>>,
}

/// PATCH /projects/:id/services/:name/settings
pub async fn update_service_settings(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
    Json(payload): Json<UpdateServiceSettingsRequest>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"}))).into_response();
    }

    // Verify project exists
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({"error": format!("{e}")})))
        .unwrap_or(None);

    if project.is_none() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": format!("project '{}' not found", project_id)}))).into_response();
    }

    // Verify service exists in project_services
    let existing: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM project_services WHERE project_id = ? AND service_name = ? LIMIT 1"
    )
    .bind(&project_id)
    .bind(&service_name)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({"error": format!("{e}")})))
    .unwrap_or(None);

    if existing.is_none() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": format!("service '{}' not found in project '{}'", service_name, project_id)}))).into_response();
    }

    let mut set_clauses: Vec<String> = Vec::new();
    let mut has_memory = false;
    let mut has_cpu = false;

    if payload.memory_limit_mb.is_some() {
        set_clauses.push("memory_limit_mb = ?".to_string());
        has_memory = true;
    }
    if payload.cpu_limit.is_some() {
        set_clauses.push("cpu_limit = ?".to_string());
        has_cpu = true;
    }

    if !has_memory && !has_cpu {
        return (StatusCode::OK, Json(serde_json::json!({"updated": true}))).into_response();
    }

    let sql = format!(
        "UPDATE project_services SET {} WHERE project_id = ? AND service_name = ?",
        set_clauses.join(", ")
    );
    let mut query = sqlx::query(&sql);

    if has_memory {
        query = query.bind(payload.memory_limit_mb.unwrap());
    }
    if has_cpu {
        query = query.bind(payload.cpu_limit.unwrap());
    }
    query = query.bind(&project_id).bind(&service_name);

    if let Err(e) = query.execute(&state.db).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")}))).into_response();
    }

    let now = chrono::Utc::now().timestamp();
    // Update project's updated_at timestamp too
    let _ = sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&project_id)
        .execute(&state.db)
        .await;

    (StatusCode::OK, Json(serde_json::json!({"updated": true}))).into_response()
}
