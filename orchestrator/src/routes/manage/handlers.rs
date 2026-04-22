use axum::{extract::Path, extract::State, http::StatusCode, Json};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::nodes;
use crate::AppState;

use super::helpers::{MessageResponse, agent_base_url, cleanup_unused_image, get_node_from_db, read_local_project_env, sync_caddy, write_local_env_snapshot};
use super::multi_service;

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
            multi_service::stop_all_services(&state, &project_id).await;
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

    // Multi-service: delegate to start_all_services
    if project.service_count.unwrap_or(1) > 1 {
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        if !is_local {
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                "multi-service start on remote nodes is not yet supported".to_string(),
            ));
        }
        multi_service::start_all_services(&state, &project).await?;
        return Ok(Json(MessageResponse {
            message: format!("project '{}' started", project_id),
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
        multi_service::delete_all_services(&state, &project_id).await;
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

    // Multi-service: stop all, remove all, then re-deploy from compose.yaml
    if project.service_count.unwrap_or(1) > 1 {
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        if !is_local {
            // Remote multi-service recreate: call agent batch-run
            let node_id = project.node_id.as_deref().unwrap();
            let node = match get_node_from_db(&state.db, node_id).await {
                Ok(n) => n,
                Err(e) => return Err((StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {:?}", e))),
            };
            let client = match nodes::client::get_node_client(&state.node_clients, node_id) {
                Ok(c) => c,
                Err(e) => return Err((StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {:?}", e))),
            };
            let base_url = agent_base_url(&state.config, &node);

            // Read compose.yaml from local disk (stored during deploy)
            let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yaml");
            let compose_yaml = match std::fs::read_to_string(&compose_path) {
                Ok(c) => c,
                Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("compose.yaml not found: {e}"))),
            };

            // Get service names from DB (agent will topo-sort from compose)
            let svc_names: Vec<String> = sqlx::query_scalar(
                "SELECT service_name FROM project_services WHERE project_id = ? ORDER BY service_name"
            )
            .bind(&project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

            let resp = match client
                .post(format!("{}/containers/batch-run", base_url))
                .json(&json!({
                    "project_id": &project_id,
                    "compose_yaml": &compose_yaml,
                    "service_order": &svc_names,
                }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}"))),
            };

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("remote recreate failed: {body}")));
            }

            // Update project_services with results from agent
            let batch_result: serde_json::Value = resp.json().await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;

            if let Some(svc_results) = batch_result["services"].as_array() {
                for svc in svc_results {
                    let svc_name = svc["service_name"].as_str().unwrap_or("");
                    let container_id = svc["container_id"].as_str();
                    let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);
                    let status = if container_id.is_some() { "running" } else { "error" };
                    let _ = sqlx::query(
                        "UPDATE project_services SET container_id = ?, mapped_port = ?, status = ? WHERE project_id = ? AND service_name = ?"
                    )
                    .bind(container_id)
                    .bind(mapped_port)
                    .bind(status)
                    .bind(&project_id)
                    .bind(svc_name)
                    .execute(&state.db)
                    .await;
                }
            }

            let _ = state.route_sync_tx.send(());

            return Ok(Json(MessageResponse {
                message: format!("project '{}' recreated on node '{}'", project_id, node_id),
            }));
        }
        return multi_service::recreate_all_services(&state, &project).await;
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
