use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use axum_login::AuthSession;
use axum::extract::Multipart;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::auth::backend::PasswordBackend;
use litebin_common::types::{Node, VolumeMount};
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

#[derive(Deserialize)]
pub struct DeployRequest {
    pub project_id: String,
    pub image: String,
    pub port: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_id: Option<String>, // optional override
    pub auto_stop_enabled: Option<bool>,
    pub auto_stop_timeout_mins: Option<i64>,
    pub auto_start_enabled: Option<bool>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub custom_domain: Option<String>,
    pub volumes: Option<Vec<VolumeMount>>,
    pub cleanup_volumes: Option<bool>,
}

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
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    if exists > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Project already exists"})),
        ).into_response();
    }

    execute_deploy(state, user_id, payload, false).await
}

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

/// Authenticate via session or deploy token.
async fn authenticate(
    auth_session: &AuthSession<PasswordBackend>,
    state: &AppState,
    headers: &HeaderMap,
    project_id: &str,
) -> Result<String, axum::response::Response> {
    match &auth_session.user {
        Some(u) => Ok(u.id.clone()),
        None => {
            match crate::auth::extract_deploy_token(state, headers, project_id).await {
                Some(uid) => Ok(uid),
                None => Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
                ).into_response()),
            }
        }
    }
}

/// Validate project ID: reserved subdomains, DNS-safe label, alias conflicts.
async fn validate_project_id(
    state: &AppState,
    payload: &DeployRequest,
) -> Option<axum::response::Response> {
    if payload.project_id == state.config.dashboard_subdomain {
        return Some((StatusCode::BAD_REQUEST, Json(json!({"error": "This ID is reserved"}))).into_response());
    }
    if payload.project_id == state.config.poke_subdomain {
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
        "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1"
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

/// Shared deploy execution logic. When `is_update` is true, uses upsert (ON CONFLICT DO UPDATE).
async fn execute_deploy(
    state: AppState,
    user_id: String,
    payload: DeployRequest,
    is_update: bool,
) -> axum::response::Response {
    let now = chrono::Utc::now().timestamp();

    let auto_stop_enabled = payload.auto_stop_enabled.unwrap_or(true);
    let auto_stop_timeout_mins = payload.auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start_enabled = payload.auto_start_enabled.unwrap_or(true);

    tracing::info!(
        project_id = %payload.project_id,
        image = %payload.image,
        port = %payload.port,
        is_update = is_update,
        "deploy request received"
    );

    // 1. Acquire deploy lock for this project_id (serializes concurrent deploys)
    let semaphore = state
        .deploy_locks
        .entry(payload.project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // 2. Capture old image before upsert (for cleanup after deploy)
    let old_image = sqlx::query_scalar::<_, String>("SELECT image FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    // 3. Insert or upsert project in DB with status='deploying'
    // Capture old volumes for orphan detection
    let old_volumes: Option<Vec<VolumeMount>> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT volumes FROM projects WHERE id = ?"
    )
    .bind(&payload.project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .and_then(|v| serde_json::from_str(&v).ok());

    let volumes_json = payload.volumes.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default());

    let result = if is_update {
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                user_id = excluded.user_id,
                image = excluded.image,
                internal_port = excluded.internal_port,
                status = 'deploying',
                name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
                description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
                auto_stop_enabled = excluded.auto_stop_enabled,
                auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
                auto_start_enabled = excluded.auto_start_enabled,
                cmd = CASE WHEN excluded.cmd IS NOT NULL THEN excluded.cmd ELSE projects.cmd END,
                memory_limit_mb = CASE WHEN excluded.memory_limit_mb IS NOT NULL THEN excluded.memory_limit_mb ELSE projects.memory_limit_mb END,
                cpu_limit = CASE WHEN excluded.cpu_limit IS NOT NULL THEN excluded.cpu_limit ELSE projects.cpu_limit END,
                custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE projects.custom_domain END,
                volumes = CASE WHEN excluded.volumes IS NOT NULL THEN excluded.volumes ELSE projects.volumes END,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&payload.project_id)
        .bind(&user_id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.image)
        .bind(payload.port)
        .bind(auto_stop_enabled)
        .bind(auto_stop_timeout_mins)
        .bind(auto_start_enabled)
        .bind(&payload.cmd)
        .bind(payload.memory_limit_mb)
        .bind(payload.cpu_limit)
        .bind(&payload.custom_domain)
        .bind(&volumes_json)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
    } else {
        // Create-only: plain INSERT, no ON CONFLICT
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&payload.project_id)
        .bind(&user_id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.image)
        .bind(payload.port)
        .bind(auto_stop_enabled)
        .bind(auto_stop_timeout_mins)
        .bind(auto_start_enabled)
        .bind(&payload.cmd)
        .bind(payload.memory_limit_mb)
        .bind(payload.cpu_limit)
        .bind(&payload.custom_domain)
        .bind(&volumes_json)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
    };

    if let Err(e) = result {
        let msg = e.to_string();
        let status = if msg.contains("UNIQUE constraint") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (
            status,
            Json(json!({"error": if msg.contains("UNIQUE constraint") { format!("project '{}' already exists", payload.project_id) } else { format!("database error: {msg}") } })),
        ).into_response();
    }

    // 3. Read project back from DB (so cmd reflects stored value, not just payload)
    let project = match sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?"
    )
    .bind(&payload.project_id)
    .fetch_one(&state.db)
    .await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("database error: {e}")})),
            ).into_response();
        }
    };

    // 4. Select target node
    let node_id = match nodes::selector::select_node(&state.db, &project, payload.node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // 5. Branch: local vs remote node
    let (container_id, mapped_port) = if node_id == "local" {
        // --- Local path ---
        crate::routes::manage::ensure_project_dir_and_env(&payload.project_id);

        // Remove any existing container for this project
        if let Err(e) = state.docker.remove_by_name(&payload.project_id).await {
            tracing::warn!(error = %e, "failed to remove old container (may not exist)");
        }

        // Pull the new image (skip if it's a local image ID, i.e. already uploaded)
        if !payload.image.starts_with("sha256:") {
            if let Err(e) = state
                .docker
                .pull_image(&payload.image)
                .await
            {
                tracing::error!(error = %e, "failed to pull image");
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to pull image: {e}")})),
            )
                .into_response();
            }
        }

        // Start the container
        let extra_env = crate::routes::manage::read_local_project_env(&payload.project_id);
        let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
        match state.docker.run_service_container(&config).await {
            Ok(result) => {
                crate::routes::manage::write_local_env_snapshot(&payload.project_id);
                result
            },
            Err(e) => {
                tracing::error!(error = %e, "failed to start container");
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to start container: {e}")})),
                )
                    .into_response();
            }
        }
    } else {
        // --- Remote node path ---

        // Get the node record for host/port
        let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) => n,
            Ok(None) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node '{}' not found", node_id)})),
                )
                    .into_response();
            }
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("database error: {e}")})),
                )
                    .into_response();
            }
        };

        // Get client from pool
        let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
            Ok(c) => c,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client not available: {e}")})),
                )
                    .into_response();
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        // Run container on agent
        let run_resp = match client
            .post(&format!("{}/containers/run", base_url))
            .json(&json!({
                "image": payload.image,
                "internal_port": payload.port,
                "project_id": payload.project_id,
                "cmd": project.cmd,
                "memory_limit_mb": project.memory_limit_mb,
                "cpu_limit": project.cpu_limit,
                "volumes": payload.volumes,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, node_id = %node_id, "failed to run container on agent");
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("agent unreachable during container run: {e}")})),
                )
                    .into_response();
            }
        };

        if !run_resp.status().is_success() {
            let status_code = run_resp.status();
            let body = run_resp.text().await.unwrap_or_default();
            tracing::error!(node_id = %node_id, status = %status_code, "container run failed");
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("container run failed: {body}")})),
            )
                .into_response();
        }

        let run_json: serde_json::Value = match run_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("invalid container run response: {e}")})),
                )
                    .into_response();
            }
        };

        let container_id = match run_json["container_id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "missing container_id in run response"})),
                )
                    .into_response();
            }
        };

        let mapped_port = match run_json["mapped_port"].as_u64() {
            Some(p) => p as u16,
            None => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "missing mapped_port in run response"})),
                )
                    .into_response();
            }
        };

        (container_id, mapped_port)
    };

    // 6. Update DB with container info and node_id
    if let Err(e) = sqlx::query(
        r#"
        UPDATE projects
        SET container_id = ?, mapped_port = ?, node_id = ?, status = 'running',
            last_active_at = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&container_id)
    .bind(mapped_port as i64)
    .bind(&node_id)
    .bind(now)
    .bind(now)
    .bind(&payload.project_id)
    .execute(&state.db)
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("database error: {e}")})),
        )
            .into_response();
    }

    // 7. Sync Caddy routes
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let route_entries = match crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve routes");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to resolve routes: {e}")})),
            )
                .into_response();
        }
    };
    if let Err(e) = state
        .router
        .read()
        .await
        .sync_routes(&route_entries, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await
    {
        tracing::error!(error = %e, "failed to sync routes — rolling back container");

        // Roll back: stop the container and reset DB status
        if node_id == "local" {
            let _ = state.docker.stop_container(&container_id).await;
            let _ = state.docker.remove_container(&container_id).await;
        } else {
            let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
                .bind(&node_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
            if let Some(node) = node {
                if let Ok(client) = nodes::client::get_node_client(&state.node_clients, &node_id) {
                    let url = agent_base_url(&state.config, &node);
                    let _ = client
                        .post(format!("{}/containers/stop", url))
                        .json(&json!({"container_id": &container_id}))
                        .send()
                        .await;
                }
            }
        }
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&payload.project_id)
            .execute(&state.db)
            .await;

        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to configure routing: {e}")})),
        )
            .into_response();
    }

    let url = format!("https://{}.{}", payload.project_id, state.config.domain);

    // 8. Clean up old image if it changed
    if let Some(ref old) = old_image {
        if old != &payload.image {
            crate::routes::manage::cleanup_unused_image(
                &state,
                Some(&node_id),
                old,
            )
            .await;
        }
    }

    tracing::info!(
        project_id = %payload.project_id,
        container_id = %container_id,
        mapped_port = %mapped_port,
        node_id = %node_id,
        url = %url,
        "deploy complete"
    );

    // 9. Detect orphaned volumes and optionally clean up
    let mut orphaned_volumes: Vec<String> = Vec::new();
    if let Some(ref old) = old_volumes {
        let new_names: std::collections::HashSet<String> = payload.volumes
            .as_ref()
            .map(|v| v.iter().map(|vm| vm.name.clone().unwrap_or_else(|| payload.project_id.clone())).collect())
            .unwrap_or_default();
        for vm in old {
            let name = vm.name.as_deref().unwrap_or(&payload.project_id);
            if !new_names.contains(name) {
                let path = std::path::PathBuf::from("projects")
                    .join(&payload.project_id)
                    .join("data")
                    .join(name);
                if path.exists() {
                    orphaned_volumes.push(path.display().to_string());
                    if payload.cleanup_volumes == Some(true) {
                        let _ = std::fs::remove_dir_all(&path);
                        tracing::info!(path = %path.display(), "cleaned up orphaned volume");
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "status": "running",
            "project_id": payload.project_id,
            "url": url,
            "custom_domain": payload.custom_domain,
            "mapped_port": mapped_port,
            "orphaned_volumes": orphaned_volumes
        })),
    )
        .into_response()
}

// ── Multi-Service (Compose) Deploy ──────────────────────────────────────────

/// POST /deploy/compose — Deploy a multi-service project via compose file.
///
/// Accepts multipart form data with:
/// - `project_id` (text field)
/// - `name` (optional text field)
/// - `description` (optional text field)
/// - `node_id` (optional text field)
/// - `auto_stop_enabled` (optional text field, "true"/"false")
/// - `auto_stop_timeout_mins` (optional text field)
/// - `auto_start_enabled` (optional text field, "true"/"false")
/// - `custom_domain` (optional text field)
/// - `compose` (file field — the docker-compose.yml content)
pub async fn deploy_compose(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Parse multipart fields
    let mut project_id = None;
    let mut name = None;
    let mut description = None;
    let mut node_id = None;
    let mut auto_stop_enabled = None;
    let mut auto_stop_timeout_mins = None;
    let mut auto_start_enabled = None;
    let mut custom_domain = None;
    let mut compose_content = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = match field.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        match field_name.as_str() {
            "project_id" => {
                project_id = field.text().await.ok();
            }
            "name" => {
                name = field.text().await.ok();
            }
            "description" => {
                description = field.text().await.ok();
            }
            "node_id" => {
                node_id = field.text().await.ok();
            }
            "auto_stop_enabled" => {
                auto_stop_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "auto_stop_timeout_mins" => {
                auto_stop_timeout_mins = field.text().await.ok().and_then(|v| v.parse::<i64>().ok());
            }
            "auto_start_enabled" => {
                auto_start_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "custom_domain" => {
                custom_domain = field.text().await.ok();
            }
            "compose" => {
                compose_content = field.bytes().await.ok();
            }
            _ => {
                tracing::debug!(field = %field_name, "ignoring unknown multipart field");
            }
        }
    }

    let project_id = match project_id {
        Some(id) if !id.is_empty() => id,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "project_id is required"})),
        ).into_response(),
    };

    let compose_bytes = match compose_content {
        Some(b) if !b.is_empty() => b,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file is required"})),
        ).into_response(),
    };

    let compose_yaml = match String::from_utf8(compose_bytes.to_vec()) {
        Ok(s) => s,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("compose file is not valid UTF-8: {e}")})),
        ).into_response(),
    };

    // Authenticate
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => {
            match crate::auth::extract_deploy_token(&state, &headers, &project_id).await {
                Some(uid) => uid,
                None => return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Authentication required"})),
                ).into_response(),
            }
        }
    };

    // Validate project ID
    if project_id == state.config.dashboard_subdomain || project_id == state.config.poke_subdomain {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "This ID is reserved"})),
        ).into_response();
    }
    if !crate::validation::is_valid_project_id(&project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens"})),
        ).into_response();
    }

    // Parse compose file
    let compose = match compose_bollard::ComposeParser::parse(&compose_yaml) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid compose YAML: {e}")})),
        ).into_response(),
    };

    if compose.services.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file has no services"})),
        ).into_response();
    }

    // 4 validation checks
    // 1. Ghost deps
    let ghosts = compose.validate_ghost_deps();
    if !ghosts.is_empty() {
        let msg = ghosts.iter()
            .map(|(svc, dep)| format!("service '{svc}' depends on unknown service '{dep}'"))
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid dependencies: {msg}")})),
        ).into_response();
    }

    // 2. Cycles
    if let Some(cycle) = compose.detect_cycles() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("dependency cycle detected: {}", cycle.join(" -> "))})),
        ).into_response();
    }

    // 3. Topological sort (also validates DAG)
    let start_order = match compose.topological_sort() {
        Ok(order) => order,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid service graph: {e}")})),
        ).into_response(),
    };

    // 4. Public service detection (warning only if none found)
    let public_service = match compose.detect_public_service() {
        Ok(s) => s,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("public service conflict: {e}")})),
        ).into_response(),
    };

    let now = chrono::Utc::now().timestamp();
    let auto_stop = auto_stop_enabled.unwrap_or(true);
    let auto_stop_mins = auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start = auto_start_enabled.unwrap_or(true);

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        public = ?public_service,
        "compose deploy request received"
    );

    // Acquire deploy lock
    let semaphore = state
        .deploy_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // Determine the public service's port for the projects row
    let public_svc_name = public_service.as_deref().unwrap_or(&start_order[0]);
    let public_svc = &compose.services[public_svc_name];
    let public_port: Option<i64> = public_svc.exposed_ports().first().map(|(p, _)| *p as i64);
    let public_image = public_svc.image.clone().unwrap_or_default();

    // Build service_count and service_summary
    let service_count = compose.services.len() as i64;
    let service_summary = start_order.join(":");

    // Upsert project row
    let result = sqlx::query(
        r#"
        INSERT INTO projects (id, user_id, name, description, image, internal_port, status,
            auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled,
            custom_domain, service_count, service_summary, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            user_id = excluded.user_id,
            image = excluded.image,
            internal_port = excluded.internal_port,
            status = 'deploying',
            name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
            description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
            auto_stop_enabled = excluded.auto_stop_enabled,
            auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
            auto_start_enabled = excluded.auto_start_enabled,
            custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE projects.custom_domain END,
            service_count = excluded.service_count,
            service_summary = excluded.service_summary,
            container_id = NULL,
            mapped_port = NULL,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&project_id)
    .bind(&user_id)
    .bind(&name)
    .bind(&description)
    .bind(&public_image)
    .bind(public_port)
    .bind(auto_stop)
    .bind(auto_stop_mins)
    .bind(auto_start)
    .bind(&custom_domain)
    .bind(service_count)
    .bind(&service_summary)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        let msg = e.to_string();
        let status = if msg.contains("UNIQUE constraint") { StatusCode::CONFLICT } else { StatusCode::INTERNAL_SERVER_ERROR };
        return (
            status,
            Json(json!({"error": if msg.contains("UNIQUE constraint") { format!("project '{}' already exists", project_id) } else { format!("database error: {msg}") } })),
        ).into_response();
    }

    // Ensure project directory exists
    crate::routes::manage::ensure_project_dir_and_env(&project_id);

    // Store compose.yml
    let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yml");
    if let Err(e) = std::fs::write(&compose_path, &compose_yaml) {
        tracing::error!(error = %e, "failed to write compose.yml");
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now).bind(&project_id).execute(&state.db).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to store compose file: {e}")})),
        ).into_response();
    }

    // Delete existing project_services and project_volumes (for redeploy)
    let _ = sqlx::query("DELETE FROM project_volumes WHERE project_id = ?")
        .bind(&project_id).execute(&state.db).await;
    let _ = sqlx::query("DELETE FROM project_services WHERE project_id = ?")
        .bind(&project_id).execute(&state.db).await;

    // Insert project_services
    for svc_name in &start_order {
        let svc = &compose.services[svc_name];
        let is_public = public_service.as_deref() == Some(svc_name.as_str());
        let port = svc.exposed_ports().first().map(|(p, _)| *p as i64);
        let depends_on = if svc.dependency_names().is_empty() {
            None
        } else {
            Some(serde_json::to_string(&svc.dependency_names()).unwrap_or_default())
        };

        let _ = sqlx::query(
            r#"
            INSERT INTO project_services (project_id, service_name, image, port, cmd, is_public,
                depends_on, memory_limit_mb, cpu_limit, status, instance_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'stopped', NULL)
            "#,
        )
        .bind(&project_id)
        .bind(svc_name)
        .bind(&svc.image)
        .bind(port)
        .bind(&svc.command.as_ref().and_then(|c| {
            if c.is_string() { c.as_str().map(String::from) }
            else { c.as_str().map(String::from) }
        }))
        .bind(is_public)
        .bind(&depends_on)
        .bind(svc.memory_bytes().map(|b| (b / (1024 * 1024)) as i64))
        .bind(svc.nano_cpus().map(|n| n as f64 / 1_000_000_000.0))
        .execute(&state.db)
        .await;

        // Insert volumes from compose
        if let Some(vols) = &svc.volumes {
            for vol_str in vols {
                // Parse "host_path:container_path[:ro]" format
                let parts: Vec<&str> = vol_str.splitn(2, ':').collect();
                if parts.len() >= 2 {
                    let container_path = parts[1].trim_end_matches(":ro").trim_end_matches(":rw");
                    let volume_name = Some(parts[0].to_string());
                    let _ = sqlx::query(
                        "INSERT OR IGNORE INTO project_volumes (project_id, service_name, volume_name, container_path) VALUES (?, ?, ?, ?)"
                    )
                    .bind(&project_id)
                    .bind(svc_name)
                    .bind(&volume_name)
                    .bind(container_path)
                    .execute(&state.db)
                    .await;
                }
            }
        }
    }

    // Select node
    let project = match sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?"
    )
    .bind(&project_id)
    .fetch_one(&state.db)
    .await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("database error: {e}")})),
            ).into_response();
        }
    };

    let target_node_id = match nodes::selector::select_node(&state.db, &project, node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now).bind(&project_id).execute(&state.db).await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": e.to_string()})),
            ).into_response();
        }
    };

    // Local deploy path
    if target_node_id != "local" {
        // Remote node support for multi-service is not yet implemented
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now).bind(&project_id).execute(&state.db).await;
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "multi-service deploy on remote nodes is not yet supported"})),
        ).into_response();
    }

    // Clean up existing containers from a previous deploy (by name prefix).
    // Handles orphaned containers from failed deploys.
    let prefix = format!("litebin-{}.", project_id);
    if let Ok(all_containers) = state.docker.list_containers_by_prefix(&prefix).await {
        for cid in &all_containers {
            let _ = state.docker.stop_container(cid).await;
            let _ = state.docker.remove_container(cid).await;
        }
    }

    // Ensure per-project network
    if let Err(e) = state.docker.ensure_project_network(&project_id, None).await {
        tracing::error!(error = %e, "failed to create project network");
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now).bind(&project_id).execute(&state.db).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to create network: {e}")})),
        ).into_response();
    }

    // Connect Caddy to the project network so it can reach containers directly
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(&project_id, None);
    if let Err(e) = state.docker.connect_container_to_network(&caddy_container, &project_network).await {
        tracing::warn!(error = %e, container = %caddy_container, network = %project_network, "failed to connect caddy to project network");
    }

    // Pull all images
    let images: Vec<String> = start_order.iter()
        .filter_map(|name| compose.services[name].image.clone())
        .collect();
    let mut pull_errors = Vec::new();
    for image in &images {
        if !image.starts_with("sha256:") {
            if let Err(e) = state.docker.pull_image(image).await {
                pull_errors.push(format!("{}: {}", image, e));
            }
        }
    }
    if !pull_errors.is_empty() {
        let msg = pull_errors.join("; ");
        tracing::error!(errors = %msg, "failed to pull images");
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now).bind(&project_id).execute(&state.db).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to pull images: {msg}")})),
        ).into_response();
    }

    // Start services in dependency order
    let extra_env = crate::routes::manage::read_local_project_env(&project_id);
    let mut started_containers: Vec<(String, String, u16)> = Vec::new(); // (service_name, container_id, mapped_port)
    let mut public_container_id = String::new();
    let mut public_mapped_port: u16 = 0;

    for svc_name in &start_order {
        let svc = &compose.services[svc_name];
        let is_public = public_service.as_deref() == Some(svc_name.as_str());

        // Build bollard config from compose service
        let bollard_config = svc.to_bollard_config(&compose_bollard::BollardMappingOptions::default());

        let run_config = litebin_common::types::RunServiceConfig {
            project_id: project_id.clone(),
            service_name: svc_name.clone(),
            instance_id: None,
            image: svc.image.clone().unwrap_or_default(),
            port: svc.exposed_ports().first().map(|(p, _)| *p),
            cmd: svc.cmd_list().map(|v| v.join(" ")),
            entrypoint: svc.entrypoint_list(),
            working_dir: svc.working_dir.clone(),
            user: svc.user.clone(),
            env: extra_env.clone(),
            memory_limit_mb: svc.memory_bytes().map(|b| (b / (1024 * 1024)) as i64),
            cpu_limit: svc.nano_cpus().map(|n| n as f64 / 1_000_000_000.0),
            shm_size: svc.shm_size.as_ref().and_then(|s| parse_compose_size(s)),
            tmpfs: None,
            read_only: svc.read_only,
            extra_hosts: svc.extra_hosts.clone(),
            networks: None,
            binds: None, // compose volumes are handled by bollard config
            is_public,
            bollard_create_body: Some(bollard_config.create_body),
            bollard_host_config: Some(bollard_config.host_config),
        };

        match state.docker.run_service_container(&run_config).await {
            Ok((container_id, mapped_port)) => {
                tracing::info!(service = %svc_name, container_id = %container_id, port = %mapped_port, "service started");

                // Wait for Docker network to assign a valid IP
                if let Err(e) = state.docker.wait_for_network_ready(&container_id).await {
                    tracing::warn!(service = %svc_name, error = %e, "network readiness timeout, continuing");
                }

                // Wait for healthcheck if this service has one defined in compose
                if svc.healthcheck.is_some() {
                    tracing::info!(service = %svc_name, "waiting for healthcheck");
                    if let Err(e) = state.docker.wait_for_healthy(&container_id, true).await {
                        tracing::warn!(service = %svc_name, error = %e, "healthcheck failed, continuing anyway");
                    } else {
                        tracing::info!(service = %svc_name, "healthcheck passed");
                    }
                }

                // Update project_services
                let _ = sqlx::query(
                    "UPDATE project_services SET container_id = ?, mapped_port = ?, status = 'running' WHERE project_id = ? AND service_name = ?"
                )
                .bind(&container_id)
                .bind(mapped_port as i64)
                .bind(&project_id)
                .bind(svc_name)
                .execute(&state.db)
                .await;

                started_containers.push((svc_name.clone(), container_id.clone(), mapped_port));

                if is_public {
                    public_container_id = container_id;
                    public_mapped_port = mapped_port;
                }
            }
            Err(e) => {
                tracing::error!(service = %svc_name, error = %e, "failed to start service");

                // Rollback: stop all started containers
                for (_, cid, _) in &started_containers {
                    let _ = state.docker.stop_container(cid).await;
                    let _ = state.docker.remove_container(cid).await;
                }

                // Reset all service statuses
                let _ = sqlx::query("UPDATE project_services SET status = 'stopped', container_id = NULL, mapped_port = NULL WHERE project_id = ?")
                    .bind(&project_id).execute(&state.db).await;
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now).bind(&project_id).execute(&state.db).await;

                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to start service '{}': {}", svc_name, e)})),
                ).into_response();
            }
        }
    }

    // Update project row with public container info
    let _ = sqlx::query(
        "UPDATE projects SET container_id = ?, mapped_port = ?, node_id = 'local', status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?"
    )
    .bind(&public_container_id)
    .bind(public_mapped_port as i64)
    .bind(now)
    .bind(now)
    .bind(&project_id)
    .execute(&state.db)
    .await;

    // Sync Caddy routes
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let route_entries = match crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve routes after compose deploy");
            // Don't fail the deploy — routes will sync on next cycle
            Vec::new()
        }
    };
    let _ = state
        .router
        .read()
        .await
        .sync_routes(&route_entries, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await;

    let url = format!("https://{}.{}", project_id, state.config.domain);

    tracing::info!(
        project_id = %project_id,
        services = started_containers.len(),
        url = %url,
        "compose deploy complete"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "running",
            "project_id": project_id,
            "url": url,
            "custom_domain": custom_domain,
            "services": started_containers.iter().map(|(name, cid, port)| json!({
                "name": name,
                "container_id": cid,
                "mapped_port": port,
            })).collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

/// Parse a compose size string ("256m", "1g") into bytes.
pub(crate) fn parse_compose_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let (num_str, multiplier) = if let Some(rest) = s.strip_suffix("gb") {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix('g') {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix("mb") {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 1024u64 * 1024)
    } else {
        return None;
    };
    let num: f64 = num_str.trim().parse().ok()?;
    Some((num * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use dashmap::DashMap;

    #[tokio::test]
    async fn prop_deploy_lock_serializes_concurrent_ops() {
        let deploy_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());
        let project_id = "test-project";

        // Create semaphore for project
        let semaphore = deploy_locks
            .entry(project_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let concurrent_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = vec![];
        for _ in 0..5 {
            let sem = semaphore.clone();
            let counter = counter.clone();
            let concurrent = concurrent_count.clone();
            let max_c = max_concurrent.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let c = concurrent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_c.fetch_max(c, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                concurrent.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 5 tasks completed
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 5);
        // Never more than 1 concurrent holder
        assert_eq!(max_concurrent.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
