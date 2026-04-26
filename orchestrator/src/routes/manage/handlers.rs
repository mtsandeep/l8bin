use axum::{extract::Path, extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use crate::AppState;

use super::helpers::{MessageResponse, agent_base_url, cleanup_unused_image, get_node_from_db, read_local_project_env, sync_caddy};
use super::multi_service::{StartServicesOpts, start_services, stop_services, recreate_services};

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

    if project.status != "running" && project.status != "degraded" {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("project is not running (status: {})", project.status),
        ));
    }

    // Branch: remote vs local
    let is_remote = project
        .node_id
        .as_deref()
        .map(|n| n != "local")
        .unwrap_or(false);

    // Set status to 'stopping' immediately and return — actual stop happens in background
    status::transition(&state.db, &project_id, "stopping", &ProjectUpdateFields::default(), None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Resync Caddy to remove the route immediately
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project stopping (async)");

    // Spawn background task to do the actual Docker stop
    let project_id_bg = project_id.clone();
    let node_id_bg = project.node_id.clone();
    tokio::spawn(async move {
        let project_id = project_id_bg;

        if is_remote {
            // Remote: call agent to stop all service containers
            let node_id = node_id_bg.unwrap_or_default();
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
            let container_ids: Vec<String> = sqlx::query_scalar(
                "SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL AND container_id != ''",
            )
            .bind(&project_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
            for cid in &container_ids {
                let _ = client
                    .post(&format!("{}/containers/stop", base_url))
                    .json(&json!({"container_id": cid}))
                    .send()
                    .await;
            }
        } else {
            // Local: stop all service containers (works for single and multi-service)
            stop_services(&state, &project_id, None).await;
        }

        let _ = status::transition(&state.db, &project_id, "stopped", &ProjectUpdateFields::default(), None).await;

        sync_caddy(&state).await;
        tracing::info!(project = %project_id, "project stopped via API");
    });

    Ok(Json(MessageResponse {
        message: format!("project '{}' stopping", project_id),
    }))
}

/// POST /projects/:id/start
/// Starts all services for a project. Uses unified start_services() which handles
/// fast-path (docker start existing) and fallback (recreate) automatically.
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

    let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);

    if is_local {
        // Unified local path: start_services handles both single and multi-service.
        // It has fast-path (docker start existing) and fallback (recreate) built in.
        start_services(&state, &project, StartServicesOpts {
            force_recreate: false,
            pull_images: false,
            services: None,
            connect_orchestrator: true,
            rollback_on_failure: false,
        }).await.map_err(|(s, e)| (s, e))?;
    } else if project.service_count.unwrap_or(1) > 1 {
        // Remote multi-service: use agent batch-run (same as deploy/recreate)
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yaml");
        let compose_yaml = std::fs::read_to_string(&compose_path)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("compose.yaml not found: {e}")))?;

        let svc_names: Vec<String> = sqlx::query_scalar(
            "SELECT service_name FROM project_services WHERE project_id = ? ORDER BY service_name"
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

        let resp = client
            .post(format!("{}/containers/batch-run", base_url))
            .json(&json!({
                "project_id": &project_id,
                "compose_yaml": &compose_yaml,
                "service_order": &svc_names,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("remote start failed: {body}")));
        }

        let batch_result: serde_json::Value = resp.json().await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;

        if let Some(svc_results) = batch_result["services"].as_array() {
            for svc in svc_results {
                let svc_name = svc["service_name"].as_str().unwrap_or("");
                let container_id = svc["container_id"].as_str();
                let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);
                if let Some(cid) = container_id {
                    let _ = status::set_service_running(&state.db, &project_id, svc_name, cid, mapped_port).await;
                } else {
                    let _ = status::set_service_stopped(&state.db, &project_id, svc_name).await;
                }
            }
        }

        status::derive_and_set_project_status(&state.db, &project_id).await;
        let _ = state.route_sync_tx.send(());
    } else {
        // Remote single-service: agent start/recreate
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let container_id = project.container_id.as_deref()
            .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?;

        let now = chrono::Utc::now().timestamp();

        // Fast path: try starting existing container
        let resp = client
            .post(&format!("{}/containers/start", base_url))
            .json(&json!({ "container_id": container_id }))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let port = r.json::<serde_json::Value>().await.ok()
                    .and_then(|v| v["mapped_port"].as_u64())
                    .map(|p| p as i64);
                status::transition(&state.db, &project_id, "running", &ProjectUpdateFields {
                    mapped_port: port.map(Some),
                    last_active_at: Some(now),
                    ..Default::default()
                }, None).await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            }
            Ok(r) => {
                tracing::warn!(project = %project_id, status = %r.status(), "agent start returned non-success, falling back to recreate");
                // Fallback: recreate on agent
                let _ = client
                    .post(format!("{}/containers/remove", base_url))
                    .json(&json!({ "container_id": container_id }))
                    .send()
                    .await;

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
                let new_cid = result["container_id"].as_str().unwrap_or("").to_string();
                let port = result["mapped_port"].as_u64()
                    .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "missing mapped_port".to_string()))? as i64;

                status::transition(&state.db, &project_id, "running", &ProjectUpdateFields {
                    container_id: Some(Some(new_cid)),
                    mapped_port: Some(Some(port)),
                    last_active_at: Some(now),
                    ..Default::default()
                }, None).await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            }
            Err(e) => {
                return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")));
            }
        }
    }

    Ok(Json(MessageResponse {
        message: format!("project '{}' started", project_id),
    }))
}

/// Build a list of scoped volume names for a project (from DB for multi-service, from JSON for single-service).
fn build_volume_list(project: &crate::db::models::Project) -> Vec<String> {
    if project.service_count.unwrap_or(1) > 1 {
        // Multi-service: volumes are in project_volumes table (already scoped at deploy time)
        // For remote delete, we pass what we have from the project record
        // The agent will discover volumes from its own state
        Vec::new()
    } else if let Some(ref vols_json) = project.volumes {
        serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(vols_json)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let name = v.name.as_deref().unwrap_or(&project.id);
                if name.starts_with('/') {
                    None // absolute bind mount — user-managed
                } else {
                    Some(litebin_common::types::scope_volume_source(name, &project.id))
                }
            })
            .collect()
    } else {
        Vec::new()
    }
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

    if is_local {
        if project.service_count.unwrap_or(1) > 1 {
            super::multi_service::delete_all_services(&state, &project_id).await;
        } else {
            // Collect volumes for single-service local cleanup
            let volumes: Vec<String> = build_volume_list(&project);
            let _ = state.docker.cleanup_project_resources(&project_id, &volumes).await;
        }
    } else {
        // Remote: call agent cleanup endpoint
        let node_id = project.node_id.as_deref().unwrap();
        let volumes = build_volume_list(&project);
        match nodes::client::get_node_client(&state.node_clients, node_id) {
            Ok(client) => {
                match get_node_from_db(&state.db, node_id).await {
                    Ok(node) => {
                        let base_url = agent_base_url(&state.config, &node);
                        let _ = client
                            .post(&format!("{}/containers/cleanup", base_url))
                            .json(&json!({
                                "project_id": project_id,
                                "volumes": volumes,
                            }))
                            .send()
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!(node_id = %node_id, error = ?e, "delete: node not found, skipping remote cleanup");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "delete: node client unavailable, skipping remote cleanup");
            }
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
/// For multi-service: accepts optional `services` array in JSON body for selective recreate.
/// Set `pull_images: true` to pull latest images before recreating (redeploy).
#[derive(Deserialize, Default)]
pub struct RecreateRequest {
    pub services: Option<Vec<String>>,
    pub pull_images: Option<bool>,
}

pub async fn recreate_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Option<Json<RecreateRequest>>,
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
                    if let Some(cid) = container_id {
                        let _ = status::set_service_running(&state.db, &project_id, svc_name, cid, mapped_port).await;
                    } else {
                        let _ = status::set_service_stopped(&state.db, &project_id, svc_name).await;
                    }
                }
            }

            let _ = state.route_sync_tx.send(());

            return Ok(Json(MessageResponse {
                message: format!("project '{}' recreated on node '{}'", project_id, node_id),
            }));
        }
        let pull = body.as_ref().and_then(|b| b.0.pull_images).unwrap_or(false);
        return recreate_services(&state, &project, body.and_then(|b| b.0.services), pull).await;
    }

    // Acquire project lock to serialize with concurrent operations
    let semaphore = state
        .project_locks
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
        status::transition(&state.db, &project_id, "running", &ProjectUpdateFields {
                container_id: Some(Some(container_id)),
                mapped_port: Some(Some(port as i64)),
                last_active_at: Some(now),
                ..Default::default()
            }, None)
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

        status::transition(&state.db, &project_id, "running", &ProjectUpdateFields {
                container_id: Some(Some(container_id)),
                mapped_port: Some(Some(mapped_port as i64)),
                last_active_at: Some(now),
                ..Default::default()
            }, None)
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

/// POST /projects/:id/services/:name/start
pub async fn start_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    start_services(&state, &project, StartServicesOpts {
        force_recreate: true,
        pull_images: false,
        services: Some(services),
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await.map_err(|(s, e)| (s, e))?;

    tracing::info!(project = %project_id, service = %service_name, "service started");

    Ok(Json(MessageResponse {
        message: format!("service '{}' started", service_name),
    }))
}

/// POST /projects/:id/services/:name/stop
pub async fn stop_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let _project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    let mut services = HashSet::new();
    services.insert(service_name.clone());
    stop_services(&state, &project_id, Some(&services)).await;

    // Derive project status from aggregate service states
    status::derive_and_set_project_status(&state.db, &project_id).await;

    sync_caddy(&state).await;
    tracing::info!(project = %project_id, service = %service_name, "service stopped");

    Ok(Json(MessageResponse {
        message: format!("service '{}' stopped", service_name),
    }))
}

/// POST /projects/:id/services/:name/restart
pub async fn restart_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    // force_recreate handles stop+remove+create, fixing the name conflict bug
    start_services(&state, &project, StartServicesOpts {
        force_recreate: true,
        pull_images: false,
        services: Some(services),
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await.map_err(|(s, e)| (s, e))?;

    tracing::info!(project = %project_id, service = %service_name, "service restarted");

    Ok(Json(MessageResponse {
        message: format!("service '{}' restarted", service_name),
    }))
}
