use std::sync::Arc;

use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use crate::routes::manage::helpers::{
    MessageResponse, agent_base_url, ensure_node_reachable, get_node_from_db, sync_caddy,
};
use crate::routes::manage::multi_service::{
    StartServicesOpts, apply_remote_batch_failure_metadata, recreate_services, start_services,
};
use crate::routes::manage::{capture_service_digests, cleanup_unused_image};

use super::shared::uses_compose_lifecycle;

/// POST /projects/:id/recreate
/// Remove and recreate the container without pulling the image.
/// Picks up updated env files from the agent's project directory.
/// For multi-service: accepts optional `services` array in JSON body for selective recreate.
/// Set `pull_images: true` to pull latest images before recreating (redeploy).
#[derive(Deserialize, Default, utoipa::ToSchema)]
pub struct RecreateRequest {
    pub services: Option<Vec<String>>,
    pub pull_images: Option<bool>,
}

#[utoipa::path(
    post,
    path = "/projects/{project_id}/recreate",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    request_body = Option<RecreateRequest>,
    responses(
        (status = 200, body = MessageResponse),
        (status = 400, description = "Bad request"),
        (status = 404, description = "Project not found"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn recreate_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Option<Json<RecreateRequest>>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if let Some(node_id) = project.node_id.as_deref().filter(|n| *n != "local") {
        ensure_node_reachable(&state, node_id).await?;
    }

    if project.status == ProjectStatus::Deploying {
        return Err((StatusCode::BAD_REQUEST, "project is already deploying".to_string()));
    }

    // Multi-service or compose: stop all, remove all, then re-deploy from compose.yaml.
    // Single-service compose projects also need this path for docker-proxy injection
    // and compose-based orchestration (env files, volumes, etc.).
    let is_compose = uses_compose_lifecycle(project.deploy_type.as_ref());
    if is_compose {
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
                "SELECT service_name FROM project_services WHERE project_id = ? ORDER BY service_name",
            )
            .bind(&project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

            // Read per-service resource overrides and global defaults to send to agent
            let service_resources: std::collections::HashMap<String, serde_json::Value> =
                sqlx::query_as::<_, (String, Option<i64>, Option<f64>)>(
                    "SELECT service_name, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ?",
                )
                .bind(&project_id)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(name, mem, cpu)| {
                    if mem.is_some() || cpu.is_some() {
                        Some((name, serde_json::json!({ "memory_limit_mb": mem, "cpu_limit": cpu })))
                    } else {
                        None
                    }
                })
                .collect();

            let default_mem: i64 =
                sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_memory_limit_mb'")
                    .fetch_one(&state.db)
                    .await
                    .ok()
                    .and_then(|v: String| v.parse().ok())
                    .unwrap_or(256);
            let default_cpu: f64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_cpu_limit'")
                .fetch_one(&state.db)
                .await
                .ok()
                .and_then(|v: String| v.parse().ok())
                .unwrap_or(0.5);
            let docker_observe = crate::capabilities::has_capability(
                &state.db,
                &project_id,
                litebin_common::capabilities::ProjectCapability::DockerObserve,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
            let host_network = crate::capabilities::has_capability(
                &state.db,
                &project_id,
                litebin_common::capabilities::ProjectCapability::HostNetwork,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
            let pull = body.as_ref().and_then(|b| b.0.pull_images).unwrap_or(false);
            let target_services = body.as_ref().and_then(|b| b.0.services.clone());
            let target_set = target_services.as_ref().map(|services| services.iter().cloned().collect());

            // Capture old image digests before batch-run (for cleanup after redeploy with pull)
            let old_digests: std::collections::HashMap<String, String> = if pull {
                capture_service_digests(&state, &project_id, Some(node_id), target_set.as_ref()).await
            } else {
                std::collections::HashMap::new()
            };

            let resp = match client
                .post(format!("{}/containers/batch-run", base_url))
                .json(&json!({
                    "project_id": &project_id,
                    "compose_yaml": &compose_yaml,
                    "service_order": &svc_names,
                    "target_services": target_services,
                    "allow_raw_ports": project.allow_raw_ports,
                    "docker_observe": docker_observe,
                    "host_network": host_network,
                    "is_background": project.is_background,
                    "service_resources": service_resources,
                    "default_memory_limit_mb": default_mem,
                    "default_cpu_limit": default_cpu,
                    "force_pull": pull,
                }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if target_services.is_some() {
                        let _ = status::set_project_error_only(&state.db, &project_id).await;
                    } else {
                        let _ = status::transition(
                            &state.db,
                            &project_id,
                            ProjectStatus::Error,
                            &ProjectUpdateFields::default(),
                            None,
                        )
                        .await;
                    }
                    return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")));
                }
            };

            if !resp.status().is_success() {
                let resp_body = match resp.text().await {
                    Ok(body) => body,
                    Err(error) => {
                        if target_services.is_some() {
                            let _ = status::set_project_error_only(&state.db, &project_id).await;
                        } else {
                            let _ = status::transition(
                                &state.db,
                                &project_id,
                                ProjectStatus::Error,
                                &ProjectUpdateFields::default(),
                                None,
                            )
                            .await;
                        }
                        return Err((
                            StatusCode::BAD_GATEWAY,
                            format!("failed to read remote recreate error response: {error}"),
                        ));
                    }
                };
                apply_remote_batch_failure_metadata(&state, &project_id, &resp_body).await;
                if target_services.is_some() {
                    let _ = status::set_project_error_only(&state.db, &project_id).await;
                } else {
                    let _ = status::transition(
                        &state.db,
                        &project_id,
                        ProjectStatus::Error,
                        &ProjectUpdateFields::default(),
                        None,
                    )
                    .await;
                }
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("remote recreate failed: {resp_body}")));
            }

            // Update project_services with results from agent
            let batch_result: serde_json::Value = match resp.json().await {
                Ok(result) => result,
                Err(e) => {
                    if target_services.is_some() {
                        let _ = status::set_project_error_only(&state.db, &project_id).await;
                    } else {
                        let _ = status::transition(
                            &state.db,
                            &project_id,
                            ProjectStatus::Error,
                            &ProjectUpdateFields::default(),
                            None,
                        )
                        .await;
                    }
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")));
                }
            };
            let service_errors: Vec<String> = batch_result["services"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|service| {
                    service["error"]
                        .as_str()
                        .map(|error| format!("{}: {}", service["service_name"].as_str().unwrap_or("unknown"), error))
                })
                .collect();

            if let Some(svc_results) = batch_result["services"].as_array() {
                for svc in svc_results {
                    let svc_name = svc["service_name"].as_str().unwrap_or("");
                    let container_id = svc["container_id"].as_str();
                    let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);
                    if let Some(cid) = container_id {
                        if let Err(e) =
                            status::set_service_running(&state.db, &project_id, svc_name, cid, mapped_port).await
                        {
                            tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "recreate: failed to set service running");
                        }
                    } else {
                        if let Err(e) = status::set_service_stopped(&state.db, &project_id, svc_name).await {
                            tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "recreate: failed to set service stopped");
                        }
                    }
                }
            }
            if !service_errors.is_empty() {
                let _ = status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await;
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("one or more services failed to recreate: {}", service_errors.join("; ")),
                ));
            }

            let _ = state.route_sync_tx.send(());

            // Clean up old images by digest after successful recreate with pull
            if !old_digests.is_empty() {
                for (_svc_name, digest) in &old_digests {
                    cleanup_unused_image(&state, Some(node_id), digest).await;
                }
            }

            let agent_warnings: Vec<String> = batch_result["warnings"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            return Ok(Json(MessageResponse {
                message: format!("project '{}' recreated on node '{}'", project_id, node_id),
                warnings: agent_warnings,
            }));
        }
        let pull = body.as_ref().and_then(|b| b.0.pull_images).unwrap_or(false);
        return recreate_services(&state, &project, body.and_then(|b| b.0.services), pull).await;
    }

    // Acquire project lock to serialize with concurrent operations
    let semaphore =
        state.project_locks.entry(project_id.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let _permit = semaphore.acquire().await.unwrap();

    let now = chrono::Utc::now().timestamp();
    let node_id = project.node_id.as_deref().unwrap_or("local");

    let image = match &project.image {
        Some(img) => img,
        None => return Err((StatusCode::BAD_REQUEST, "project has no image deployed yet".to_string())),
    };
    let internal_port = project.internal_port;
    let docker_observe = crate::capabilities::has_capability(
        &state.db,
        &project_id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
    let volumes = project
        .volumes
        .as_deref()
        .and_then(|volumes| serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(volumes).ok());

    let is_remote = node_id != "local";

    // For remote: recreate on agent (auto-assigns port)
    let mapped_port = if is_remote {
        let node = get_node_from_db(&state.db, node_id).await?;
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let base_url = agent_base_url(&state.config, &node);

        // Remove existing container on agent
        if let Err(e) = client
            .post(format!("{}/containers/remove", base_url))
            .json(&json!({
                "container_id": project.container_id,
            }))
            .send()
            .await
        {
            tracing::warn!(project_id = %project_id, error = %e, "recreate: failed to remove old container on agent");
        }

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
                "volumes": volumes,
                "docker_observe": docker_observe,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("recreate failed: {body}")));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;
        let container_id = result["container_id"].as_str().unwrap_or("").to_string();
        let port = result["mapped_port"].as_u64().unwrap_or(0) as u16;

        // Update DB
        status::transition(
            &state.db,
            &project_id,
            ProjectStatus::Running,
            &ProjectUpdateFields {
                container_id: Some(Some(container_id)),
                mapped_port: Some(if project.is_background { None } else { Some(port as i64) }),
                last_active_at: Some(now),
                ..Default::default()
            },
            None,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

        port
    } else {
        start_services(
            &state,
            &project,
            StartServicesOpts {
                force_recreate: true,
                pull_images: false,
                force_pull: false,
                services: None,
                connect_orchestrator: true,
                rollback_on_failure: true,
            },
        )
        .await?;
        sqlx::query_scalar::<_, Option<i64>>("SELECT mapped_port FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
            .unwrap_or(0) as u16
    };

    sync_caddy(&state).await;

    tracing::info!(project = %project_id, port = %mapped_port, "project recreated");

    Ok(Json(MessageResponse { message: format!("project '{}' recreated", project_id), ..Default::default() }))
}
