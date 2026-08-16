use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::{ProjectStatus, VolumeMount};

use super::types::DeployRequest;

/// The background deploy task: pull, start the container, sync routes, and
/// clean up old images/volumes. On failure the project is transitioned to Error
/// (handled by the caller).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_deploy_task(
    state_clone: AppState,
    payload_clone: DeployRequest,
    project_clone: crate::db::models::Project,
    node_id_clone: String,
    old_image_clone: Option<String>,
    old_node_id_clone: Option<String>,
    old_volumes_clone: Option<Vec<VolumeMount>>,
    is_background_clone: bool,
    granted_capabilities: Vec<litebin_common::capabilities::ProjectCapability>,
) -> Result<(), anyhow::Error> {
    // Capture old image digest before any destructive operations
    let old_digest = if let Some(ref old) = old_image_clone {
        crate::routes::manage::get_image_digest(&state_clone, old_node_id_clone.as_deref(), old).await
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
        crate::routes::manage::start_services(
            &state_clone,
            &project_clone,
            crate::routes::manage::StartServicesOpts {
                force_recreate: true,
                pull_images: false,
                force_pull: false,
                services: None,
                connect_orchestrator: true,
                rollback_on_failure: true,
            },
        )
        .await
        .map_err(|(_, error)| anyhow::anyhow!(error))?;
        let result: (String, i64) =
            sqlx::query_as("SELECT container_id, COALESCE(mapped_port, 0) FROM projects WHERE id = ?")
                .bind(&payload_clone.project_id)
                .fetch_one(&state_clone.db)
                .await?;
        crate::routes::deploy::logs::push_deploy_log(
            &state_clone,
            &payload_clone.project_id,
            &format!("Container started on port {}", result.1),
        );
        (result.0, result.1 as u16)
    } else {
        // --- Remote node path ---
        crate::routes::deploy::logs::push_deploy_log(
            &state_clone,
            &payload_clone.project_id,
            &format!("Deploying to remote node {}...", &node_id_clone),
        );
        let agent = nodes::client::AgentClient::resolve(&state_clone, &node_id_clone).await?;

        let run_request = litebin_common::agent_api::RunRequest {
            image: payload_clone.image.clone(),
            internal_port: payload_clone.port,
            project_id: payload_clone.project_id.clone(),
            cmd: project_clone.cmd.clone(),
            memory_limit_mb: project_clone.memory_limit_mb,
            cpu_limit: project_clone.cpu_limit,
            volumes: project_clone
                .volumes
                .as_deref()
                .and_then(|volumes| serde_json::from_str::<Vec<VolumeMount>>(volumes).ok()),
            docker_observe: granted_capabilities
                .contains(&litebin_common::capabilities::ProjectCapability::DockerObserve),
            stage_only: false,
        };

        let run_resp = match agent.run(&run_request).await {
            Ok(resp) => resp,
            Err(nodes::client::AgentClientError::Status { body, .. }) => {
                anyhow::bail!("agent container run failed: {body}");
            }
            Err(e) => return Err(e.into()),
        };
        (run_resp.container_id, run_resp.mapped_port.unwrap_or(0))
    };

    // 6. Update DB with container info
    status::transition(
        &state_clone.db,
        &payload_clone.project_id,
        ProjectStatus::Running,
        &ProjectUpdateFields {
            container_id: Some(Some(container_id.clone())),
            mapped_port: Some(if is_background_clone { None } else { Some(mapped_port as i64) }),
            node_id: Some(node_id_clone.clone()),
            last_active_at: Some(chrono::Utc::now().timestamp()),
        },
        None,
    )
    .await?;

    // Create project_services row for single-service deploy
    sqlx::query(
        "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit)
         VALUES (?, 'web', ?, ?, ?, ?, 'running', ?, ?, ?, ?)",
    )
    .bind(&payload_clone.project_id)
    .bind(&payload_clone.image)
    .bind(payload_clone.port)
    .bind(if is_background_clone { None } else { Some(mapped_port as i64) })
    .bind(!is_background_clone)
    .bind(&container_id)
    .bind(&project_clone.cmd)
    .bind(project_clone.memory_limit_mb)
    .bind(project_clone.cpu_limit)
    .execute(&state_clone.db)
    .await?;

    // 7. Sync Caddy routes
    crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Syncing routes...");
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state_clone.config.port);
    let route_entries = crate::routing_helpers::resolve_all_routes(
        &state_clone.db,
        &state_clone.platform.domain(),
        &orchestrator_upstream,
    )
    .await?;
    if let Err(e) = state_clone
        .router
        .read()
        .await
        .sync_routes(
            &route_entries,
            &state_clone.platform.domain(),
            &orchestrator_upstream,
            &state_clone.platform.dashboard_subdomain(),
            &state_clone.platform.poke_subdomain(),
            true,
        )
        .await
    {
        tracing::error!(error = %e, "failed to sync routes — rolling back container");

        // Roll back: stop the container and reset DB status
        if node_id_clone == "local" {
            let _ = state_clone.docker.stop_container(&container_id).await;
            let _ = state_clone.docker.remove_container(&container_id).await;
        } else {
            if let Ok(agent) = nodes::client::AgentClient::resolve(&state_clone, &node_id_clone).await
                && let Err(e) =
                    agent.stop(&litebin_common::agent_api::StopRequest { container_id: container_id.clone() }).await
            {
                tracing::warn!(project_id = %payload_clone.project_id, container_id = %container_id, error = %e, "deploy: failed to stop container on agent");
            }
        }
        if let Err(e) = status::transition(
            &state_clone.db,
            &payload_clone.project_id,
            ProjectStatus::Error,
            &ProjectUpdateFields::default(),
            None,
        )
        .await
        {
            tracing::warn!(project_id = %payload_clone.project_id, error = %e, "deploy: failed to transition to Error");
        }
        anyhow::bail!("failed to configure routing: {}", e);
    }

    // 8. Clean up old image by digest (handles same-tag redeploy)
    if let Some(ref digest) = old_digest {
        crate::routes::manage::cleanup_unused_image(&state_clone, old_node_id_clone.as_deref(), digest).await;
    }
    // Fallback: clean up by old tag if it changed (in case digest lookup failed)
    if let Some(ref old) = old_image_clone
        && old != &payload_clone.image
        && old_digest.is_none()
    {
        crate::routes::manage::cleanup_unused_image(&state_clone, old_node_id_clone.as_deref(), old).await;
    }

    // 9. Detect orphaned volumes and optionally clean up
    if let Some(ref old) = old_volumes_clone {
        let new_names: std::collections::HashSet<String> = payload_clone
            .volumes
            .as_ref()
            .map(|v: &Vec<litebin_common::types::VolumeMount>| {
                v.iter().map(|vm| vm.name.clone().unwrap_or_else(|| payload_clone.project_id.clone())).collect()
            })
            .unwrap_or_default();
        for vm in old {
            let name = vm.name.as_deref().unwrap_or(&payload_clone.project_id);
            if !new_names.contains(name) {
                let scoped = litebin_common::types::scope_volume_source(name, &payload_clone.project_id);
                if litebin_common::types::classify_volume(&scoped)
                    == litebin_common::types::VolumeKind::AbsoluteBindMount
                {
                    continue;
                }
                if payload_clone.cleanup_volumes == Some(true) {
                    let _ = state_clone.docker.remove_volume_by_name(&scoped).await;
                    tracing::info!(volume = %scoped, "cleaned up orphaned volume");
                }
            }
        }
    }

    tracing::info!(
        project_id = %payload_clone.project_id,
        container_id = %container_id,
        mapped_port = %mapped_port,
        node_id = %node_id_clone,
        "deploy complete"
    );

    crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Routes synced");
    crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Deployment complete");
    crate::routes::deploy::logs::clear_deploy_logs(&state_clone, &payload_clone.project_id);

    // Trigger route sync for downstream consumers
    let _ = state_clone.route_sync_tx.send(());

    Ok(())
}
