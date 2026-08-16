use std::sync::Arc;

use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use crate::routes::manage::helpers::{MessageResponse, ensure_node_reachable, sync_caddy};
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
            let agent = nodes::client::AgentClient::resolve(&state, node_id)
                .await
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;

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

            let payload = crate::routes::manage::multi_service::build_batch_run_payload(
                &state.db,
                crate::routes::manage::multi_service::BatchRunInputs {
                    project_id: project_id.clone(),
                    compose_yaml,
                    service_order: svc_names,
                    target_services: target_services.clone(),
                    allow_raw_ports: Some(project.allow_raw_ports),
                    docker_observe: Some(docker_observe),
                    host_network: Some(host_network),
                    is_background: project.is_background,
                    force_pull: pull,
                    stage_only: false,
                },
            )
            .await;
            let batch_result = match agent.batch_run(&payload).await {
                Ok(result) => result,
                Err(nodes::client::AgentClientError::Status { body: resp_body, .. }) => {
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
                    return Err(e.into_response_parts());
                }
            };
            let service_errors: Vec<String> = batch_result
                .services
                .iter()
                .filter_map(|svc| svc.error.as_deref().map(|error| format!("{}: {error}", svc.service_name)))
                .collect();

            for svc in &batch_result.services {
                let mapped_port = svc.mapped_port.map(i64::from);
                if let Some(cid) = svc.container_id.as_deref() {
                    if let Err(e) =
                        status::set_service_running(&state.db, &project_id, &svc.service_name, cid, mapped_port).await
                    {
                        tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "recreate: failed to set service running");
                    }
                } else {
                    if let Err(e) = status::set_service_stopped(&state.db, &project_id, &svc.service_name).await {
                        tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "recreate: failed to set service stopped");
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
                for digest in old_digests.values() {
                    cleanup_unused_image(&state, Some(node_id), digest).await;
                }
            }

            return Ok(Json(MessageResponse {
                message: format!("project '{}' recreated on node '{}'", project_id, node_id),
                warnings: batch_result.warnings,
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
        let agent = nodes::client::AgentClient::resolve(&state, node_id)
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;

        // Remove existing container on agent
        if let Some(old_container_id) = project.container_id.as_deref()
            && let Err(e) = agent
                .remove(&litebin_common::agent_api::RemoveRequest { container_id: old_container_id.to_string() })
                .await
        {
            tracing::warn!(project_id = %project_id, error = %e, "recreate: failed to remove old container on agent");
        }

        // Recreate container on agent (no pull, auto-assign port)
        let run_request = litebin_common::agent_api::RunRequest {
            image: image.to_string(),
            internal_port,
            project_id: project_id.clone(),
            cmd: project.cmd.clone(),
            memory_limit_mb: project.memory_limit_mb,
            cpu_limit: project.cpu_limit,
            volumes: volumes.clone(),
            docker_observe,
            stage_only: false,
        };
        let result = match agent.recreate(&run_request).await {
            Ok(result) => result,
            Err(nodes::client::AgentClientError::Status { body, .. }) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("recreate failed: {body}")));
            }
            Err(e) => return Err(e.into_response_parts()),
        };
        let container_id = result.container_id;
        let port = result.mapped_port.unwrap_or(0);

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
