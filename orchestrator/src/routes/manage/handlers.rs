use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::{DeployType, ProjectStatus};

use super::helpers::{
    MessageResponse, agent_base_url, cleanup_unused_image, get_node_from_db, project_is_staged, sync_caddy,
};
use super::multi_service::{
    StartServicesOpts, approved_docker_observe_requesters, proxy_needed_after_stop, recreate_services, start_services,
    stop_services,
};

fn can_attempt_full_stop(status: &ProjectStatus) -> bool {
    matches!(status, ProjectStatus::Running | ProjectStatus::Degraded | ProjectStatus::Stopping | ProjectStatus::Error)
}

fn uses_compose_lifecycle(deploy_type: Option<&DeployType>) -> bool {
    deploy_type == Some(&DeployType::Compose)
}

/// POST /projects/:id/stop
#[utoipa::path(
    post,
    path = "/projects/{project_id}/stop",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 400, description = "Bad request"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn stop_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if !can_attempt_full_stop(&project.status) {
        return Err((StatusCode::BAD_REQUEST, format!("project is not running (status: {})", project.status)));
    }

    // Branch: remote vs local
    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    // Set status to 'stopping' immediately and return — actual stop happens in background
    status::set_project_stopping_only(&state.db, &project_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Resync Caddy to remove the route immediately
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project stopping (async)");

    // Spawn background task to do the actual Docker stop
    let semaphore =
        state.project_locks.entry(project_id.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let project_id_bg = project_id.clone();
    let node_id_bg = project.node_id.clone();
    tokio::spawn(async move {
        let _permit = semaphore.acquire().await.unwrap();
        let project_id = project_id_bg;

        let stop_result: Result<(), String> = async {
            if is_remote {
                // Remote: let the agent select workloads by project identity so a
                // replacement with an unpersisted container ID is still stopped.
                let node_id = node_id_bg.unwrap_or_default();
                let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
                    Ok(c) => c,
                    Err(e) => {
                        return Err(format!("node client unavailable: {e}"));
                    }
                };
                let node = match get_node_from_db(&state.db, &node_id).await {
                    Ok(n) => n,
                    Err(e) => {
                        return Err(format!("failed to get node: {e:?}"));
                    }
                };
                let base_url = agent_base_url(&state.config, &node);
                let response = client
                    .post(&format!("{}/containers/stop-project", base_url))
                    .json(&json!({"project_id": &project_id}))
                    .send()
                    .await
                    .map_err(|e| format!("agent project stop response unavailable: {e}"))?;
                if !response.status().is_success() {
                    return Err(format!("agent project stop failed: {}", response.text().await.unwrap_or_default()));
                }
                status::set_non_oneshot_services_stopped(&state.db, &project_id)
                    .await
                    .map_err(|e| format!("failed to persist stopped services: {e}"))?;
                status::derive_and_set_project_status(&state.db, &project_id).await;
            } else {
                // Local: stop all service containers (works for single and multi-service)
                stop_services(&state, &project_id, None).await.map_err(|(_, error)| error)?;
            }
            Ok(())
        }
        .await;

        let status_result = if stop_result.is_ok() {
            let derived = status::derive_and_set_project_status(&state.db, &project_id).await;
            if derived == ProjectStatus::Stopped {
                status::set_project_stopped_only(&state.db, &project_id).await
            } else {
                let _ = status::set_project_error_only(&state.db, &project_id).await;
                Err(anyhow::anyhow!("project remained {derived} after all stop operations completed"))
            }
        } else {
            status::set_project_error_only(&state.db, &project_id).await
        };
        if let Err(e) = status_result {
            tracing::warn!(project_id = %project_id, error = %e, "stop: failed to persist final project status");
        }
        if let Err(error) = stop_result {
            tracing::error!(project_id = %project_id, %error, "project stop failed");
        }

        sync_caddy(&state).await;
        tracing::info!(project = %project_id, "project stopped via API");
    });

    Ok(Json(MessageResponse { message: format!("project '{}' stopping", project_id), ..Default::default() }))
}

/// POST /projects/:id/start
/// Starts all services for a project. Uses unified start_services() which handles
/// fast-path (docker start existing) and fallback (recreate) automatically.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/start",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 400, description = "Bad request"),
        (status = 404, description = "Project not found"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn start_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status == ProjectStatus::Running {
        return Ok(Json(MessageResponse {
            message: format!("project '{}' is already running", project_id),
            ..Default::default()
        }));
    }

    if project.status == ProjectStatus::Pending {
        return Err((StatusCode::BAD_REQUEST, format!("project '{}' has not been staged yet", project_id)));
    }

    if project.status == ProjectStatus::Unconfigured {
        if !project_is_staged(&project) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("project '{}' has no staged deployment data yet", project_id),
            ));
        }
        status::transition(&state.db, &project_id, ProjectStatus::Deploying, &ProjectUpdateFields::default(), None)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    }

    let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
    let is_compose = uses_compose_lifecycle(project.deploy_type.as_ref());
    let first_start = project.status == ProjectStatus::Unconfigured;

    if is_local {
        // It has fast-path (docker start existing) and fallback (recreate) built in.
        // First start after staging always recreates and pulls registry images.
        start_services(
            &state,
            &project,
            StartServicesOpts {
                force_recreate: first_start,
                pull_images: first_start,
                force_pull: false,
                services: None,
                connect_orchestrator: true,
                rollback_on_failure: false,
            },
        )
        .await
        .map_err(|(s, e)| (s, e))?;
    } else if is_compose {
        // Remote multi-service: use agent batch-run (same as deploy/recreate)
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yaml");
        let compose_yaml = std::fs::read_to_string(&compose_path)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("compose.yaml not found: {e}")))?;

        let svc_names: Vec<String> =
            sqlx::query_scalar("SELECT service_name FROM project_services WHERE project_id = ? ORDER BY service_name")
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

        let default_mem: i64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_memory_limit_mb'")
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
        let resp = match client
            .post(format!("{}/containers/batch-run", base_url))
            .json(&json!({
                "project_id": &project_id,
                "compose_yaml": &compose_yaml,
                "service_order": &svc_names,
                "allow_raw_ports": project.allow_raw_ports,
                "docker_observe": docker_observe,
                "host_network": host_network,
                "is_background": project.is_background,
                "service_resources": service_resources,
                "default_memory_limit_mb": default_mem,
                "default_cpu_limit": default_cpu,
                "force_pull": false,
            }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                let _ = status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await;
                return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")));
            }
        };

        if !resp.status().is_success() {
            let body = match resp.text().await {
                Ok(body) => body,
                Err(error) => {
                    let _ = status::transition(
                        &state.db,
                        &project_id,
                        ProjectStatus::Error,
                        &ProjectUpdateFields::default(),
                        None,
                    )
                    .await;
                    return Err((
                        StatusCode::BAD_GATEWAY,
                        format!("failed to read remote start error response: {error}"),
                    ));
                }
            };
            super::multi_service::apply_remote_batch_failure_metadata(&state, &project_id, &body).await;
            let _ =
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("remote start failed: {body}")));
        }

        let batch_result: serde_json::Value = match resp.json().await {
            Ok(result) => result,
            Err(e) => {
                let _ = status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await;
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
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "start: failed to set service running");
                    }
                } else {
                    if let Err(e) = status::set_service_stopped(&state.db, &project_id, svc_name).await {
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "start: failed to set service stopped");
                    }
                }
            }
        }
        if !service_errors.is_empty() {
            let _ =
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await;
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("one or more services failed to start: {}", service_errors.join("; ")),
            ));
        }

        status::derive_and_set_project_status(&state.db, &project_id).await;
        let _ = state.route_sync_tx.send(());
    } else {
        // Remote single-service: agent start/recreate/run
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);
        let now = chrono::Utc::now().timestamp();

        let image = project.image.as_deref().ok_or((StatusCode::BAD_REQUEST, "project has no image".to_string()))?;
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

        // Observation-enabled services are recreated so their private proxy and
        // network are restored together with the workload.
        if project.container_id.is_none() || docker_observe {
            let resp = client
                .post(format!("{}/containers/run", base_url))
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
                let _ = status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await;
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("remote run failed: {body}")));
            }

            let result: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?;
            let new_cid = result["container_id"].as_str().unwrap_or("").to_string();
            let port = result["mapped_port"].as_u64().map(|p| p as i64);

            status::transition(
                &state.db,
                &project_id,
                ProjectStatus::Running,
                &ProjectUpdateFields {
                    container_id: Some(Some(new_cid.clone())),
                    mapped_port: Some(port),
                    last_active_at: Some(now),
                    ..Default::default()
                },
                None,
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

            if let Err(e) = sqlx::query(
                "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit)
                 VALUES (?, 'web', ?, ?, ?, ?, 'running', ?, ?, ?, ?)",
            )
            .bind(&project_id)
            .bind(image)
            .bind(internal_port)
            .bind(port)
            .bind(!project.is_background)
            .bind(&new_cid)
            .bind(&project.cmd)
            .bind(project.memory_limit_mb)
            .bind(project.cpu_limit)
            .execute(&state.db)
            .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "start: failed to upsert project_services row");
            }

            let _ = state.route_sync_tx.send(());
        } else {
            let container_id = project.container_id.as_deref().unwrap();

            // Fast path: try starting existing container
            let resp = client
                .post(&format!("{}/containers/start", base_url))
                .json(&json!({ "container_id": container_id }))
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let port = r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["mapped_port"].as_u64())
                        .map(|p| p as i64);
                    status::transition(
                        &state.db,
                        &project_id,
                        ProjectStatus::Running,
                        &ProjectUpdateFields {
                            mapped_port: Some(if project.is_background { None } else { port }),
                            last_active_at: Some(now),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
                }
                Ok(r) => {
                    tracing::warn!(project = %project_id, status = %r.status(), "agent start returned non-success, falling back to recreate");
                    // Fallback: recreate on agent
                    if let Err(e) = client
                        .post(format!("{}/containers/remove", base_url))
                        .json(&json!({ "container_id": container_id }))
                        .send()
                        .await
                    {
                        tracing::warn!(project_id = %project_id, container_id = %container_id, error = %e, "start: failed to remove old container on agent");
                    }

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
                    let new_cid = result["container_id"].as_str().unwrap_or("").to_string();
                    let port = result["mapped_port"].as_u64().map(|p| p as i64);

                    status::transition(
                        &state.db,
                        &project_id,
                        ProjectStatus::Running,
                        &ProjectUpdateFields {
                            container_id: Some(Some(new_cid)),
                            mapped_port: Some(port),
                            last_active_at: Some(now),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
                }
                Err(e) => {
                    return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")));
                }
            }
        }
    }

    Ok(Json(MessageResponse { message: format!("project '{}' started", project_id), ..Default::default() }))
}

/// Build a list of scoped volume names for a project (from DB for multi-service, from JSON for single-service).
async fn build_volume_list(db: &sqlx::SqlitePool, project: &crate::db::models::Project) -> Vec<String> {
    if uses_compose_lifecycle(project.deploy_type.as_ref()) {
        match sqlx::query_as::<_, (String,)>(
            "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(db)
        .await
        {
            Ok(rows) => rows.into_iter().map(|(name,)| name).collect(),
            Err(e) => {
                tracing::warn!(project = %project.id, error = %e, "delete: failed to fetch project volumes");
                Vec::new()
            }
        }
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
#[utoipa::path(
    delete,
    path = "/projects/{project_id}",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn delete_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    // Remove container(s) — branch on node location
    let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);

    if is_local {
        if uses_compose_lifecycle(project.deploy_type.as_ref()) {
            super::multi_service::delete_all_services(&state, &project_id).await;
        } else {
            // Collect volumes for single-service local cleanup
            let volumes: Vec<String> = build_volume_list(&state.db, &project).await;
            let _ = state.docker.cleanup_project_resources(&project_id, &volumes).await;
        }
    } else {
        // Remote: call agent cleanup endpoint
        let node_id = project.node_id.as_deref().unwrap();
        let volumes = build_volume_list(&state.db, &project).await;
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);
        let response = client
            .post(&format!("{}/containers/cleanup", base_url))
            .json(&json!({
                "project_id": project_id,
                "volumes": volumes,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent cleanup failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err((StatusCode::BAD_GATEWAY, format!("agent cleanup returned {status}: {body}")));
        }
    }

    // Clean up all per-service images if no longer in use
    let service_images: Vec<String> = sqlx::query_scalar("SELECT image FROM project_services WHERE project_id = ?")
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    let unique_images: std::collections::HashSet<String> = service_images.into_iter().collect();
    for image in &unique_images {
        cleanup_unused_image(&state, project.node_id.as_deref(), image).await;
    }

    // Delete from DB
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Clear in-memory deploy logs
    crate::routes::deploy::logs::clear_deploy_logs(&state, &project_id);

    // Resync Caddy routes
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project deleted via API");

    Ok(Json(MessageResponse { message: format!("project '{}' deleted", project_id), ..Default::default() }))
}

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
                crate::routes::manage::capture_service_digests(&state, &project_id, Some(node_id), target_set.as_ref())
                    .await
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
                super::multi_service::apply_remote_batch_failure_metadata(&state, &project_id, &resp_body).await;
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
                    crate::routes::manage::cleanup_unused_image(&state, Some(node_id), digest).await;
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

/// POST /projects/:id/services/:name/start
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/start",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn start_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.node_id.as_deref().is_some_and(|node| node != "local") {
        return recreate_project(
            State(state),
            Path(project_id),
            Some(Json(RecreateRequest { services: Some(vec![service_name]), pull_images: Some(false) })),
        )
        .await;
    }

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    start_services(
        &state,
        &project,
        StartServicesOpts {
            force_recreate: true,
            pull_images: false,
            force_pull: false,
            services: Some(services),
            connect_orchestrator: true,
            rollback_on_failure: false,
        },
    )
    .await
    .map_err(|(s, e)| (s, e))?;

    tracing::info!(project = %project_id, service = %service_name, "service started");

    Ok(Json(MessageResponse { message: format!("service '{}' started", service_name), ..Default::default() }))
}

/// POST /projects/:id/services/:name/stop
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/stop",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn stop_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if let Some(node_id) = project.node_id.as_deref().filter(|node| *node != "local") {
        let requesters = approved_docker_observe_requesters(&state, &project).await?;
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let response = client
            .post(format!("{}/containers/stop-service", agent_base_url(&state.config, &node)))
            .json(&json!({
                "project_id": &project_id,
                "service_name": &service_name,
            }))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("remote service stop failed: {}", response.text().await.unwrap_or_default()),
            ));
        }
        status::set_service_stopped(&state.db, &project_id, &service_name)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
        status::derive_and_set_project_status(&state.db, &project_id).await;
        let running_services: HashSet<String> = sqlx::query_scalar(
            "SELECT service_name FROM project_services WHERE project_id = ? AND status IN ('running', 'stopping')",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .into_iter()
        .collect();
        let no_additional_stops = HashSet::new();
        if !proxy_needed_after_stop(&requesters, &running_services, Some(&no_additional_stops)) {
            let client = nodes::client::get_node_client(&state.node_clients, node_id)
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
            let node = get_node_from_db(&state.db, node_id).await?;
            let proxy_name =
                litebin_common::types::container_name(&project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None);
            let response = client
                .post(format!("{}/containers/remove", agent_base_url(&state.config, &node)))
                .json(&json!({ "container_id": proxy_name }))
                .send()
                .await
                .map_err(|e| {
                    (StatusCode::SERVICE_UNAVAILABLE, format!("failed to remove Docker observation proxy: {e}"))
                })?;
            if !response.status().is_success() {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!(
                        "remote Docker observation proxy cleanup failed: {}",
                        response.text().await.unwrap_or_default()
                    ),
                ));
            }
        }
    } else {
        let mut services = HashSet::new();
        services.insert(service_name.clone());
        stop_services(&state, &project_id, Some(&services)).await?;
    }

    // Derive project status from aggregate service states
    status::derive_and_set_project_status(&state.db, &project_id).await;

    sync_caddy(&state).await;
    tracing::info!(project = %project_id, service = %service_name, "service stopped");

    Ok(Json(MessageResponse { message: format!("service '{}' stopped", service_name), ..Default::default() }))
}

/// POST /projects/:id/services/:name/restart
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/restart",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn restart_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.node_id.as_deref().is_some_and(|node| node != "local") {
        return recreate_project(
            State(state),
            Path(project_id),
            Some(Json(RecreateRequest { services: Some(vec![service_name]), pull_images: Some(false) })),
        )
        .await;
    }

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    // force_recreate handles stop+remove+create, fixing the name conflict bug
    start_services(
        &state,
        &project,
        StartServicesOpts {
            force_recreate: true,
            pull_images: false,
            force_pull: false,
            services: Some(services),
            connect_orchestrator: true,
            rollback_on_failure: false,
        },
    )
    .await
    .map_err(|(s, e)| (s, e))?;

    tracing::info!(project = %project_id, service = %service_name, "service restarted");

    Ok(Json(MessageResponse { message: format!("service '{}' restarted", service_name), ..Default::default() }))
}

#[cfg(test)]
mod tests {
    use super::{can_attempt_full_stop, uses_compose_lifecycle};
    use litebin_common::types::{DeployType, ProjectStatus};

    #[test]
    fn full_stop_accepts_retryable_runtime_states_only() {
        for status in [ProjectStatus::Running, ProjectStatus::Degraded, ProjectStatus::Stopping, ProjectStatus::Error] {
            assert!(can_attempt_full_stop(&status), "{status}");
        }

        for status in [
            ProjectStatus::Pending,
            ProjectStatus::Stopped,
            ProjectStatus::Deploying,
            ProjectStatus::Importing,
            ProjectStatus::Unconfigured,
            ProjectStatus::Completed,
        ] {
            assert!(!can_attempt_full_stop(&status), "{status}");
        }
    }

    #[test]
    fn one_service_lifecycle_routing_uses_deploy_type_not_service_count() {
        // Service count is deliberately not an input to this decision.
        assert!(uses_compose_lifecycle(Some(&DeployType::Compose)));
        assert!(!uses_compose_lifecycle(Some(&DeployType::Image)));
        assert!(!uses_compose_lifecycle(None));

        // Full stop is deliberately identity-based and remains retryable for
        // both deployment types, including a one-service background Compose.
        assert!(can_attempt_full_stop(&ProjectStatus::Running));
        assert!(can_attempt_full_stop(&ProjectStatus::Error));
    }
}
