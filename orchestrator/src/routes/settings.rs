use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

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
