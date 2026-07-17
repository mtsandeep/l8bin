use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::auth::backend::PasswordBackend;
use litebin_common::types::{Node, VolumeMount};
use crate::nodes;
use crate::routes::manage::agent_base_url;
use litebin_common::types::ProjectStatus;
use crate::status::{self, ProjectUpdateFields};
use crate::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeployResponse {
    pub status: String,
    pub project_id: String,
    pub message: String,
}

#[derive(Deserialize, Clone, utoipa::ToSchema)]
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
    /// When true on a first deploy, persist project metadata and create the runtime
    /// `.env` without starting containers. Ignored for redeploys of configured projects.
    #[serde(default)]
    pub stage_only: bool,
}

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
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Project already exists"})),
        ).into_response();
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

    // On redeploy, preserve existing sleep settings unless explicitly provided
    let preserve_sleep = is_update && payload.auto_stop_enabled.is_none() && payload.auto_start_enabled.is_none();
    let (auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled) = if preserve_sleep {
        let existing = sqlx::query_as::<_, (bool, i64, bool)>(
            "SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?"
        )
        .bind(&payload.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        match existing {
            Some((s, t, a)) => (s, t, a),
            None => (auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled),
        }
    } else {
        (auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled)
    };

    let existing_status: Option<ProjectStatus> = sqlx::query_scalar(
        "SELECT status FROM projects WHERE id = ?"
    )
    .bind(&payload.project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let stage_only = payload.stage_only
        && matches!(existing_status, None | Some(ProjectStatus::Unconfigured));

    tracing::info!(
        project_id = %payload.project_id,
        image = %payload.image,
        port = %payload.port,
        is_update = is_update,
        stage_only = stage_only,
        "deploy request received"
    );

    // 1. Acquire deploy lock for this project_id (serializes concurrent deploys)
    let semaphore = state
        .project_locks
        .entry(payload.project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // 2. Capture old image and node before upsert (for cleanup after deploy)
    let (old_image, old_node_id) = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT image, node_id FROM projects WHERE id = ?"
    )
    .bind(&payload.project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or((None, None));

    let initial_status = if stage_only {
        ProjectStatus::Unconfigured
    } else {
        ProjectStatus::Deploying
    };

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
    .and_then(|v| match serde_json::from_str(&v) {
        Ok(mounts) => Some(mounts),
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse old volumes JSON, skipping volume diff");
            None
        }
    });

    let volumes_json = payload.volumes.as_ref().and_then(|v| litebin_common::types::serialize_volumes(v));

    let result = if is_update {
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                user_id = excluded.user_id,
                image = excluded.image,
                internal_port = excluded.internal_port,
                status = ?,
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
        .bind(initial_status.clone())
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
        .bind(initial_status.clone())
        .execute(&state.db)
        .await
    } else {
        // Create-only: plain INSERT, no ON CONFLICT
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&payload.project_id)
        .bind(&user_id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.image)
        .bind(payload.port)
        .bind(initial_status.clone())
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
        let is_conflict = crate::validation::is_unique_constraint(&e);
        let status = if is_conflict {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (
            status,
            Json(json!({"error": if is_conflict { format!("project '{}' already exists", payload.project_id) } else { format!("database error: {e}") } })),
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
            if let Err(e) = status::transition(&state.db, &payload.project_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None).await {
                tracing::warn!(project_id = %payload.project_id, error = %e, "deploy: failed to transition to Stopped");
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Persist sticky node selection for staged and live deploys.
    if let Err(e) = sqlx::query(
        "UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?"
    )
    .bind(&node_id)
    .bind(now)
    .bind(&payload.project_id)
    .execute(&state.db)
    .await
    {
        tracing::warn!(project_id = %payload.project_id, error = %e, "deploy: failed to persist node_id");
    }

    // First-deploy staging: create runtime .env / metadata, do not start containers.
    if stage_only {
        if node_id == "local" {
            crate::routes::manage::ensure_project_dir_and_env(&payload.project_id);
        } else {
            let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
                .bind(&node_id)
                .fetch_optional(&state.db)
                .await
            {
                Ok(Some(n)) => n,
                Ok(None) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("node '{}' not found", node_id)})),
                    ).into_response();
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": format!("database error: {e}")})),
                    ).into_response();
                }
            };

            let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
                Ok(c) => c,
                Err(e) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("node client unavailable: {e}")})),
                    ).into_response();
                }
            };
            let base_url = agent_base_url(&state.config, &node);
            let stage_resp = match client
                .post(&format!("{}/containers/run", base_url))
                .json(&json!({
                    "image": payload.image,
                    "internal_port": payload.port,
                    "project_id": payload.project_id,
                    "cmd": project.cmd,
                    "memory_limit_mb": project.memory_limit_mb,
                    "cpu_limit": project.cpu_limit,
                    "volumes": payload.volumes,
                    "stage_only": true,
                }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": format!("agent unreachable: {e}")})),
                    ).into_response();
                }
            };

            if !stage_resp.status().is_success() {
                let body = stage_resp.text().await.unwrap_or_default();
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("remote stage failed: {body}")})),
                ).into_response();
            }
        }

        tracing::info!(
            project_id = %payload.project_id,
            node_id = %node_id,
            "deployment staged; awaiting runtime configuration"
        );

        return (
            StatusCode::OK,
            Json(json!({
                "status": "unconfigured",
                "project_id": payload.project_id,
                "node_id": node_id,
                "url": format!("https://{}.{}", payload.project_id, state.config.domain),
                "message": "Deployment staged. Configure runtime secrets, then start the project.",
            })),
        ).into_response();
    }

    // 5. Spawn background task for heavy lifting (Pull, Start, Route Sync)
    let state_clone = state.clone();
    let payload_clone = payload.clone();
    let project_clone = project.clone();
    let node_id_clone = node_id.clone();
    let old_image_clone = old_image.clone();
    let old_node_id_clone = old_node_id.clone();
    let old_volumes_clone = old_volumes.clone();

    tokio::spawn(async move {
        crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Deployment started");

        let result: Result<(), anyhow::Error> = async {
            // Capture old image digest before any destructive operations
            let old_digest = if let Some(ref old) = old_image_clone {
                crate::routes::manage::get_image_digest(
                    &state_clone, old_node_id_clone.as_deref(), old,
                ).await
            } else {
                None
            };

            // 5a. Pull image / start container
            let (container_id, mapped_port) = if node_id_clone == "local" {
                // --- Local path ---
                crate::routes::manage::ensure_project_dir_and_env(&payload_clone.project_id);

                // Remove any existing container for this project
                if let Err(e) = state_clone.docker.remove_by_name(&payload_clone.project_id).await {
                    tracing::warn!(error = %e, "failed to remove old container (may not exist)");
                }

                // Pull the new image (skip if it's a local image ID or already exists locally)
                if !payload_clone.image.starts_with("sha256:") {
                    let log_state = state_clone.clone();
                    let log_project_id = payload_clone.project_id.clone();
                    let on_progress: Box<dyn Fn(&str) + Send + Sync> = Box::new(move |msg: &str| {
                        crate::routes::deploy::logs::push_deploy_log(&log_state, &log_project_id, msg);
                    });
                    state_clone.docker.pull_image_with_progress(&payload_clone.image, false, Some(on_progress)).await?;
                }

                // Start the container
                crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Creating container...");
                let extra_env = crate::routes::manage::read_local_project_env(&payload_clone.project_id);
                let config = litebin_common::types::RunServiceConfig::from_project(&project_clone, extra_env);
                let result = state_clone.docker.run_service_container(&config).await?;
                crate::routes::manage::write_local_env_snapshot(&payload_clone.project_id);
                crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, &format!("Container started on port {}", result.1));
                result
            } else {
                // --- Remote node path ---
                crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, &format!("Deploying to remote node {}...", &node_id_clone));
                let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
                    .bind(&node_id_clone)
                    .fetch_optional(&state_clone.db)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("node '{}' not found", node_id_clone))?;

                let client = nodes::client::get_node_client(&state_clone.node_clients, &node_id_clone)?;
                let base_url = agent_base_url(&state_clone.config, &node);

                let run_resp = client
                    .post(&format!("{}/containers/run", base_url))
                    .json(&json!({
                        "image": payload_clone.image,
                        "internal_port": payload_clone.port,
                        "project_id": payload_clone.project_id,
                        "cmd": project_clone.cmd,
                        "memory_limit_mb": project_clone.memory_limit_mb,
                        "cpu_limit": project_clone.cpu_limit,
                        "volumes": payload_clone.volumes,
                    }))
                    .send()
                    .await?;

                if !run_resp.status().is_success() {
                    let body = run_resp.text().await.unwrap_or_default();
                    anyhow::bail!("agent container run failed: {}", body);
                }

                let run_json: serde_json::Value = run_resp.json().await?;
                let cid = run_json["container_id"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing container_id in run response"))?
                    .to_string();
                let port = run_json["mapped_port"].as_u64()
                    .ok_or_else(|| anyhow::anyhow!("missing mapped_port in run response"))? as u16;
                (cid, port)
            };

            // 6. Update DB with container info
            status::transition(
                &state_clone.db,
                &payload_clone.project_id,
                ProjectStatus::Running,
                &ProjectUpdateFields {
                    container_id: Some(Some(container_id.clone())),
                    mapped_port: Some(Some(mapped_port as i64)),
                    node_id: Some(node_id_clone.clone()),
                    last_active_at: Some(chrono::Utc::now().timestamp()),
                },
                None,
            ).await?;

            // Create project_services row for single-service deploy
            sqlx::query(
                "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit)
                 VALUES (?, 'web', ?, ?, ?, 1, 'running', ?, ?, ?, ?)",
            )
            .bind(&payload_clone.project_id)
            .bind(&payload_clone.image)
            .bind(payload_clone.port)
            .bind(mapped_port as i64)
            .bind(&container_id)
            .bind(&project_clone.cmd)
            .bind(project_clone.memory_limit_mb)
            .bind(project_clone.cpu_limit)
            .execute(&state_clone.db)
            .await?;

            // 7. Sync Caddy routes
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Syncing routes...");
            let orchestrator_upstream = format!("litebin-orchestrator:{}", state_clone.config.port);
            let route_entries = crate::routing_helpers::resolve_all_routes(&state_clone.db, &state_clone.config.domain, &orchestrator_upstream).await?;
            if let Err(e) = state_clone
                .router
                .read()
                .await
                .sync_routes(&route_entries, &state_clone.config.domain, &orchestrator_upstream, &state_clone.config.dashboard_subdomain, &state_clone.config.poke_subdomain, true)
                .await
            {
                tracing::error!(error = %e, "failed to sync routes — rolling back container");

                // Roll back: stop the container and reset DB status
                if node_id_clone == "local" {
                    let _ = state_clone.docker.stop_container(&container_id).await;
                    let _ = state_clone.docker.remove_container(&container_id).await;
                } else {
                    if let Some(node) = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
                        .bind(&node_id_clone)
                        .fetch_optional(&state_clone.db)
                        .await
                        .ok()
                        .flatten()
                    {
                        if let Ok(client) = nodes::client::get_node_client(&state_clone.node_clients, &node_id_clone) {
                            let url = agent_base_url(&state_clone.config, &node);
                            if let Err(e) = client
                                .post(format!("{}/containers/stop", url))
                                .json(&json!({"container_id": &container_id}))
                                .send()
                                .await
                            {
                                tracing::warn!(project_id = %payload_clone.project_id, container_id = %container_id, error = %e, "deploy: failed to stop container on agent");
                            }
                        }
                    }
                }
                if let Err(e) = status::transition(&state_clone.db, &payload_clone.project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await {
                    tracing::warn!(project_id = %payload_clone.project_id, error = %e, "deploy: failed to transition to Error");
                }
                anyhow::bail!("failed to configure routing: {}", e);
            }

            // 8. Clean up old image by digest (handles same-tag redeploy)
            if let Some(ref digest) = old_digest {
                crate::routes::manage::cleanup_unused_image(
                    &state_clone,
                    old_node_id_clone.as_deref(),
                    digest,
                ).await;
            }
            // Fallback: clean up by old tag if it changed (in case digest lookup failed)
            if let Some(ref old) = old_image_clone {
                if old != &payload_clone.image && old_digest.is_none() {
                    crate::routes::manage::cleanup_unused_image(
                        &state_clone,
                        old_node_id_clone.as_deref(),
                        old,
                    ).await;
                }
            }

            // 9. Detect orphaned volumes and optionally clean up
            if let Some(ref old) = old_volumes_clone {
                let new_names: std::collections::HashSet<String> = payload_clone.volumes
                    .as_ref()
                    .map(|v: &Vec<litebin_common::types::VolumeMount>| v.iter().map(|vm| vm.name.clone().unwrap_or_else(|| payload_clone.project_id.clone())).collect())
                    .unwrap_or_default();
                for vm in old {
                    let name = vm.name.as_deref().unwrap_or(&payload_clone.project_id);
                    if !new_names.contains(name) {
                        let scoped = litebin_common::types::scope_volume_source(name, &payload_clone.project_id);
                        match litebin_common::types::classify_volume(&scoped) {
                            litebin_common::types::VolumeKind::AbsoluteBindMount => continue,
                            _ => {}
                        }
                        if payload_clone.cleanup_volumes == Some(true) {
                            let _ = state_clone.docker.remove_volume_by_name(&scoped).await;
                            tracing::info!(volume = %scoped, "cleaned up orphaned volume");
                        }
                    }
                }
            }

            let url = format!("https://{}.{}", payload_clone.project_id, state_clone.config.domain);
            tracing::info!(
                project_id = %payload_clone.project_id,
                container_id = %container_id,
                mapped_port = %mapped_port,
                node_id = %node_id_clone,
                url = %url,
                "deploy complete"
            );

            crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Routes synced");
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Deployment complete");
            crate::routes::deploy::logs::clear_deploy_logs(&state_clone, &payload_clone.project_id);

            // Trigger route sync for downstream consumers
            let _ = state_clone.route_sync_tx.send(());

            Ok(())
        }.await;

        if let Err(e) = result {
            tracing::error!(project_id = %payload_clone.project_id, error = %e, "background deploy failed");
            crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, &format!("Deploy failed: {}", e));
            let _ = status::transition(&state_clone.db, &payload_clone.project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await;
        }
    });

    (
        StatusCode::OK,
        Json(json!({
            "status": "deploying",
            "project_id": payload.project_id,
            "url": format!("https://{}.{}", payload.project_id, state.config.domain),
            "message": "Deployment started in background"
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use dashmap::DashMap;

    #[tokio::test]
    async fn prop_deploy_lock_serializes_concurrent_ops() {
        let project_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());
        let project_id = "test-project";

        // Create semaphore for project
        let semaphore = project_locks
            .entry(project_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();

        // Acquire permit
        let sem = semaphore.clone();
        let permit1 = sem.acquire().await.unwrap();
        assert!(semaphore.available_permits() == 0);

        // Second acquire should wait (drop first to verify)
        drop(permit1);
        let permit2 = sem.acquire().await.unwrap();
        assert!(semaphore.available_permits() == 0);
        drop(permit2);
    }
}
