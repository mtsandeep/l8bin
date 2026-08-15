use std::sync::Arc;

use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use crate::routes::manage::helpers::{
    MessageResponse, agent_base_url, ensure_node_reachable, get_node_from_db, project_is_staged, sync_caddy,
};
use crate::routes::manage::multi_service::{
    StartServicesOpts, apply_remote_batch_failure_metadata, start_services, stop_services,
};

use super::shared::{can_attempt_full_stop, uses_compose_lifecycle};

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

    if let Some(node_id) = project.node_id.as_deref().filter(|n| *n != "local") {
        ensure_node_reachable(&state, node_id).await?;
    }

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
                    .post(format!("{}/containers/stop-project", base_url))
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

    if let Some(node_id) = project.node_id.as_deref().filter(|n| *n != "local") {
        ensure_node_reachable(&state, node_id).await?;
    }

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
        .await?;
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
            apply_remote_batch_failure_metadata(&state, &project_id, &body).await;
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
                .post(format!("{}/containers/start", base_url))
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
