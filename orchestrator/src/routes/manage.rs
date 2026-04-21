use axum::{extract::Path, extract::State, http::StatusCode, Json};
use serde::Serialize;
use serde_json::json;
use std::hash::Hasher;
use std::sync::Arc;
use tokio::sync::Semaphore;

use litebin_common::types::Node;
use crate::nodes;
use crate::AppState;

#[derive(Serialize)]
pub struct MessageResponse {
    pub message: String,
}

/// Build the base URL for an agent node.
pub fn agent_base_url(config: &crate::config::Config, node: &Node) -> String {
    if config.ca_cert_path.is_empty() {
        format!("http://{}:{}", node.host, node.agent_port)
    } else {
        format!("https://{}:{}", node.host, node.agent_port)
    }
}

/// Fetch a Node record from the DB by node_id.
pub async fn get_node_from_db(
    db: &sqlx::SqlitePool,
    node_id: &str,
) -> Result<Node, (StatusCode, String)> {
    sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("node '{}' not found", node_id)))
}

/// POST /projects/:id/stop
pub async fn stop_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status != "running" {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("project is not running (status: {})", project.status),
        ));
    }

    let container_id = project
        .container_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?
        .to_string();

    // Branch: remote vs local
    let is_remote = project
        .node_id
        .as_deref()
        .map(|n| n != "local")
        .unwrap_or(false);

    // Set status to 'stopping' immediately and return — actual stop happens in background
    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE projects SET status = 'stopping', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Resync Caddy to remove the route immediately
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project stopping (async)");

    // Spawn background task to do the actual Docker stop
    let project_id_bg = project_id.clone();
    let service_count = project.service_count.unwrap_or(1);
    tokio::spawn(async move {
        let project_id = project_id_bg;

        if service_count > 1 {
            // Multi-service: stop all service containers in reverse dependency order
            let services: Vec<(String, Option<String>, Option<String>)> = match sqlx::query_as(
                "SELECT service_name, container_id, depends_on FROM project_services WHERE project_id = ? AND status = 'running'",
            )
            .bind(&project_id)
            .fetch_all(&state.db)
            .await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(project = %project_id, error = %e, "stop: failed to fetch services");
                    return;
                }
            };

            // Simple approach: stop in reverse of the fetched order (dependencies first)
            for (svc_name, cid, _) in services.iter().rev() {
                if let Some(container_id) = cid {
                    let _ = state.docker.stop_container(container_id).await;
                    let _ = sqlx::query(
                        "UPDATE project_services SET status = 'stopped', container_id = NULL, mapped_port = NULL WHERE project_id = ? AND service_name = ?"
                    )
                    .bind(&project_id)
                    .bind(svc_name)
                    .execute(&state.db)
                    .await;
                    tracing::info!(project = %project_id, service = %svc_name, "service stopped");
                }
            }
        } else {
            // Single-service: stop the one container
            if is_remote {
                let node_id = project.node_id.as_deref().unwrap().to_string();
                let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(project = %project_id, error = %e, "stop: node client unavailable");
                        return;
                    }
                };
                let node = match get_node_from_db(&state.db, &node_id).await {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!(project = %project_id, error = ?e, "stop: failed to get node");
                        return;
                    }
                };
                let base_url = agent_base_url(&state.config, &node);
                let _ = client
                    .post(&format!("{}/containers/stop", base_url))
                    .json(&json!({"container_id": container_id}))
                    .send()
                    .await;
            } else {
                let _ = state.docker.stop_container(&container_id).await;
            }
        }

        let now = chrono::Utc::now().timestamp();
        let _ = if service_count > 1 {
            sqlx::query("UPDATE projects SET status = 'stopped', container_id = NULL, mapped_port = NULL, updated_at = ? WHERE id = ?")
        } else {
            sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
        }
        .bind(now)
        .bind(&project_id)
        .execute(&state.db)
        .await;

        sync_caddy(&state).await;
        tracing::info!(project = %project_id, "project stopped via API");
    });

    Ok(Json(MessageResponse {
        message: format!("project '{}' stopping", project_id),
    }))
}

/// POST /projects/:id/start
/// Tries to start the existing stopped container (fast path).
/// Falls back to recreating with a new port if the old port is taken.
pub async fn start_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status == "running" {
        return Ok(Json(MessageResponse {
            message: format!("project '{}' is already running", project_id),
        }));
    }

    let container_id = project
        .container_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?;

    let is_remote = project
        .node_id
        .as_deref()
        .map(|n| n != "local")
        .unwrap_or(false);

    let now = chrono::Utc::now().timestamp();

    // Fast path: try starting the existing container (port still baked in)
    let (started, remote_mapped_port) = if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let resp = client
            .post(&format!("{}/containers/start", base_url))
            .json(&json!({ "container_id": container_id }))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let port = r.json::<serde_json::Value>().await.ok()
                    .and_then(|v| v["mapped_port"].as_u64())
                    .map(|p| p as u16);
                (true, port)
            }
            _ => (false, None),
        }
    } else {
        let ok = state.docker.start_existing_container(container_id).await.is_ok();
        (ok, None)
    };

    if started {
        // Local: inspect Docker for actual mapped port
        let actual_port = if !is_remote {
            state.docker.inspect_mapped_port(container_id).await.ok()
        } else {
            remote_mapped_port
        };

        if let Some(port) = actual_port {
            sqlx::query(
                "UPDATE projects SET status = 'running', mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
            )
            .bind(port as i64)
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            tracing::info!(project = %project_id, port = %port, "project started (fast path, port synced)");
        } else {
            sqlx::query(
                "UPDATE projects SET status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?",
            )
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            tracing::warn!(project = %project_id, "project started but could not read mapped port");
        }

        sync_caddy(&state).await;

        return Ok(Json(MessageResponse {
            message: format!("project '{}' started", project_id),
        }));
    }

    // Fallback: port likely taken — recreate container with a new port
    tracing::info!(project = %project_id, "start failed, recreating with new port");

    if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        // Remove old container
        let _ = client
            .post(format!("{}/containers/remove", base_url))
            .json(&json!({
                "container_id": container_id,
            }))
            .send()
            .await;

        // Recreate — agent will auto-assign a port
        let image = project.image.as_deref()
            .ok_or((StatusCode::BAD_REQUEST, "project has no image".to_string()))?;
        let internal_port = project.internal_port
            .ok_or((StatusCode::BAD_REQUEST, "project has no port configured".to_string()))?;

        let resp = client
            .post(format!("{}/containers/recreate", base_url))
            .json(&json!({
                "image": image,
                "internal_port": internal_port,
                "project_id": project_id,
                "cmd": project.cmd,
                "memory_limit_mb": project.memory_limit_mb,
                "cpu_limit": project.cpu_limit,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("recreate failed: {body}")));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;
        let new_container_id = result["container_id"].as_str().unwrap_or("").to_string();
        let mapped_port = result["mapped_port"].as_u64()
            .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "missing mapped_port in response".to_string()))? as u16;

        sqlx::query("UPDATE projects SET container_id = ?, mapped_port = ?, status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?")
            .bind(&new_container_id)
            .bind(mapped_port as i64)
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    } else {
        // Local: remove old, create fresh
        let _ = state.docker.remove_container(container_id).await;

        let project_clone = {
            let mut p = project.clone();
            p.container_id = None;
            p.mapped_port = None;
            p
        };

        let extra_env = read_local_project_env(&project_id);

        let config = litebin_common::types::RunServiceConfig::from_project(&project_clone, extra_env);
        let (new_container_id, mapped_port) = state.docker.run_service_container(&config).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to recreate: {e}")))?;

        write_local_env_snapshot(&project_id);

        sqlx::query("UPDATE projects SET container_id = ?, mapped_port = ?, status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?")
            .bind(&new_container_id)
            .bind(mapped_port as i64)
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    }

    sync_caddy(&state).await;
    tracing::info!(project = %project_id, "project started via API (recreate fallback)");

    Ok(Json(MessageResponse {
        message: format!("project '{}' started", project_id),
    }))
}

/// DELETE /projects/:id
pub async fn delete_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    // Remove container(s) — branch on node location
    let is_local = project
        .node_id
        .as_deref()
        .map(|n| n == "local")
        .unwrap_or(true);

    if project.service_count.unwrap_or(1) > 1 && is_local {
        // Multi-service local: remove all service containers + per-project network
        let services: Vec<(String, Option<String>)> = match sqlx::query_as(
            "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(project = %project_id, error = %e, "delete: failed to fetch services");
                Vec::new()
            }
        };

        for (svc_name, cid) in &services {
            if let Some(container_id) = cid {
                let _ = state.docker.stop_container(container_id).await;
                let _ = state.docker.remove_container(container_id).await;
                tracing::info!(project = %project_id, service = %svc_name, "service container removed during delete");
            }
        }

        // Remove per-project network
        let _ = state.docker.remove_project_network(&project_id, None).await;
    } else if let Some(ref container_id) = project.container_id {
        let is_remote = project
            .node_id
            .as_deref()
            .map(|n| n != "local")
            .unwrap_or(false);

        if is_remote {
            let node_id = project.node_id.as_deref().unwrap();
            match nodes::client::get_node_client(&state.node_clients, node_id) {
                Ok(client) => {
                    match get_node_from_db(&state.db, node_id).await {
                        Ok(node) => {
                            let base_url = agent_base_url(&state.config, &node);
                            let _ = client
                                .post(&format!("{}/containers/remove", base_url))
                                .json(&json!({
                                    "container_id": container_id,
                                }))
                                .send()
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(node_id = %node_id, error = ?e, "delete: node not found, skipping remote remove");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(node_id = %node_id, error = %e, "delete: node client unavailable, skipping remote remove");
                }
            }
        } else {
            let _ = state.docker.remove_container(container_id).await;
        }
    }

    // Clean up the project's image if no longer in use
    if let Some(ref image) = project.image {
        cleanup_unused_image(&state, project.node_id.as_deref(), image).await;
    }

    // Delete from DB
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Resync Caddy routes
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project deleted via API");

    Ok(Json(MessageResponse {
        message: format!("project '{}' deleted", project_id),
    }))
}

/// POST /projects/:id/recreate
/// Remove and recreate the container without pulling the image.
/// Picks up updated env files from the agent's project directory.
pub async fn recreate_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status == "deploying" {
        return Err((
            StatusCode::BAD_REQUEST,
            "project is already deploying".to_string(),
        ));
    }

    // Acquire deploy lock to serialize with concurrent deploys
    let semaphore = state
        .deploy_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    let now = chrono::Utc::now().timestamp();
    let node_id = project.node_id.as_deref().unwrap_or("local");

    let image = match &project.image {
        Some(img) => img,
        None => return Err((StatusCode::BAD_REQUEST, "project has no image deployed yet".to_string())),
    };
    let internal_port = match project.internal_port {
        Some(p) => p,
        None => return Err((StatusCode::BAD_REQUEST, "project has no port configured yet".to_string())),
    };

    let is_remote = node_id != "local";

    // For remote: recreate on agent (auto-assigns port)
    let mapped_port = if is_remote {
        let node = get_node_from_db(&state.db, node_id).await?;
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let base_url = agent_base_url(&state.config, &node);

        // Remove existing container on agent
        let _ = client
            .post(format!("{}/containers/remove", base_url))
            .json(&json!({
                "container_id": project.container_id,
            }))
            .send()
            .await;

        // Recreate container on agent (no pull, auto-assign port)
        let resp = client
            .post(format!("{}/containers/recreate", base_url))
            .json(&json!({
                "image": image,
                "internal_port": internal_port,
                "project_id": project_id,
                "cmd": project.cmd,
                "memory_limit_mb": project.memory_limit_mb,
                "cpu_limit": project.cpu_limit,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("recreate failed: {body}")));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;
        let container_id = result["container_id"].as_str().unwrap_or("").to_string();
        let port = result["mapped_port"].as_u64()
            .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "missing mapped_port in response".to_string()))? as u16;

        // Update DB
        sqlx::query("UPDATE projects SET container_id = ?, mapped_port = ?, status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?")
            .bind(&container_id)
            .bind(port as i64)
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

        port
    } else {
        // Local path: remove old container, recreate without pull
        let _ = state.docker.remove_by_name(&project_id).await;

        let extra_env = read_local_project_env(&project_id);

        let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
        let (container_id, mapped_port) = state.docker.run_service_container(&config).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to recreate: {e}")))?;

        sqlx::query("UPDATE projects SET container_id = ?, mapped_port = ?, status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?")
            .bind(&container_id)
            .bind(mapped_port as i64)
            .bind(now)
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

        mapped_port
    };

    sync_caddy(&state).await;

    tracing::info!(project = %project_id, port = %mapped_port, "project recreated");

    Ok(Json(MessageResponse {
        message: format!("project '{}' recreated", project_id),
    }))
}


/// Ensure the project-specific directory exists and has a placeholder .env if missing.
pub fn ensure_project_dir_and_env(project_id: &str) {
    let project_dir = std::path::PathBuf::from("projects").join(project_id);
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        tracing::error!(project = project_id, error = %e, "failed to create project directory");
        return;
    }

    let env_path = project_dir.join(".env");
    if !env_path.exists() {
        let placeholder = "# Place your runtime environment variables here\n# These variables are injected directly into your container at startup.\n";
        if let Err(e) = std::fs::write(&env_path, placeholder) {
            tracing::error!(project = project_id, error = %e, "failed to create placeholder .env");
        } else {
            tracing::info!(project = project_id, path = %env_path.display(), "created placeholder .env");
        }
    }
}

/// Read env vars from the local project .env file.
pub fn read_local_project_env(project_id: &str) -> Vec<String> {
    // Ensure the directory and placeholder exist
    ensure_project_dir_and_env(project_id);

    let env_path = std::path::PathBuf::from("projects").join(project_id).join(".env");
    if !env_path.exists() {
        return Vec::new();
    }
    match dotenvy::from_path_iter(&env_path) {
        Ok(iter) => iter
            .filter_map(|item| item.ok())
            .map(|(k, v)| format!("{}={}", k, v))
            .collect(),
        Err(e) => {
            tracing::warn!(project = project_id, error = %e, "failed to parse .env");
            Vec::new()
        }
    }
}

/// Hash a file's raw content. Returns 0 if the file doesn't exist.
fn file_hash(path: &std::path::Path) -> u64 {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&bytes, &mut hasher);
            hasher.finish()
        }
        Err(_) => 0,
    }
}

/// Hash the content portion of a .env.l8bin snapshot (strips the 5-line header).
/// Returns 0 if the file doesn't exist.
fn snapshot_content_hash(path: &std::path::Path) -> u64 {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    // Skip the header: 4 comment lines + 1 blank line
    let payload: String = content.lines().skip(5).collect::<Vec<_>>().join("\n");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&payload, &mut hasher);
    hasher.finish()
}

/// Check if the project .env has changed since the last container creation.
pub fn local_env_has_changed(project_id: &str) -> bool {
    let env_path = std::path::PathBuf::from("projects").join(project_id).join(".env");
    let snapshot_path = std::path::PathBuf::from("projects").join(project_id).join(".env.l8bin");

    let env_hash = file_hash(&env_path);
    // .env.l8bin has a 5-line header (4 comments + blank line) — strip it before hashing
    let snapshot_hash = snapshot_content_hash(&snapshot_path);

    let changed = env_hash != snapshot_hash;
    tracing::info!(project = project_id, env_hash = env_hash, snapshot_hash = snapshot_hash, changed = changed, "env change check");
    changed
}

/// Write .env.l8bin snapshot — a copy of the current .env with a header.
/// Called after successfully creating a container with injected env vars.
pub fn write_local_env_snapshot(project_id: &str) {
    let env_path = std::path::PathBuf::from("projects").join(project_id).join(".env");
    let snapshot_path = std::path::PathBuf::from("projects").join(project_id).join(".env.l8bin");

    let content = match std::fs::read_to_string(&env_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let header = "# Auto-generated by LiteBin — do not edit manually.\n\
        # This records the env vars last injected into your container.\n\
        # Compare with .env to see pending changes.\n\
        # Docs: https://github.com/mtsandeep/l8bin/blob/main/docs/env-secrets.md\n";

    if let Err(e) = std::fs::write(&snapshot_path, format!("{}\n{}", header, content)) {
        tracing::warn!(project = project_id, error = %e, "failed to write .env.l8bin snapshot");
    } else {
        tracing::info!(project = project_id, "wrote .env.l8bin snapshot");
    }
}

/// Remove an image from the node if no container is using it.
/// All errors are logged and swallowed — cleanup must never block the caller.
pub async fn cleanup_unused_image(state: &AppState, node_id: Option<&str>, image: &str) {
    let node_id = node_id.unwrap_or("local");
    if node_id == "local" {
        match state.docker.remove_unused_image(image).await {
            Ok(true) => tracing::info!(image = %image, "cleaned up unused local image"),
            Ok(false) => {}
            Err(e) => tracing::warn!(image = %image, error = %e, "failed to clean up local image"),
        }
    } else {
        let client = match nodes::client::get_node_client(&state.node_clients, node_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "cleanup: node client unavailable");
                return;
            }
        };
        let node = match get_node_from_db(&state.db, node_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = ?e, "cleanup: node not found");
                return;
            }
        };
        let base_url = agent_base_url(&state.config, &node);
        match client
            .post(format!("{}/images/remove-unused", base_url))
            .json(&json!({ "image": image }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(image = %image, node_id = %node_id, "cleaned up unused remote image");
            }
            Ok(resp) => {
                tracing::warn!(image = %image, node_id = %node_id, status = %resp.status(), "cleanup: agent returned non-success");
            }
            Err(e) => {
                tracing::warn!(image = %image, node_id = %node_id, error = %e, "cleanup: agent unreachable");
            }
        }
    }
}

pub async fn sync_caddy(state: &AppState) {
    let routes = match crate::routing_helpers::resolve_all_routes(
        &state.db, &state.config.domain, &format!("litebin-orchestrator:{}", state.config.port),
    ).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve routes");
            return;
        }
    };

    let upstream = format!("litebin-orchestrator:{}", state.config.port);
    if let Err(e) = state
        .router
        .read()
        .await
        .sync_routes(&routes, &state.config.domain, &upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await
    {
        tracing::error!(error = %e, "failed to sync routes");
    }
}
