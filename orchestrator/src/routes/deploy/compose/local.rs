use std::collections::HashMap;

use crate::AppState;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

/// The background local deploy task: remove existing containers (full or
/// targeted), pull images with progress logs, start services, sync routes,
/// and clean up old per-service images.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_local_compose_deploy(
    state_clone: AppState,
    project_id_clone: String,
    project_clone: crate::db::models::Project,
    compose_clone: compose_bollard::ComposeFile,
    start_order_clone: Vec<String>,
    target_node_id_clone: String,
    target_services_clone: Option<Vec<String>>,
    old_service_digests_clone: HashMap<String, String>,
    existing_node_id_clone: Option<String>,
) -> Result<(), anyhow::Error> {
    // Partial redeploy: only recreate targeted services
    if let Some(ref targets) = target_services_clone {
        tracing::info!(project = %project_id_clone, targets = ?targets, "partial compose redeploy");

        let target_set: std::collections::HashSet<String> = targets.iter().cloned().collect();

        // Stop and remove targeted service containers
        let prefix = format!("litebin-{}.", project_id_clone);
        if let Ok(all_containers) = state_clone.docker.list_containers_by_prefix(&prefix).await {
            for cid in &all_containers {
                if let Ok(inspect) = state_clone.docker.inspect_container(cid).await
                    && let Some(ref name) = inspect.name
                {
                    let trimmed = name.trim_start_matches('/');
                    if let Some(svc_name) = trimmed.strip_prefix(&prefix)
                        && target_set.contains(svc_name)
                    {
                        let _ = state_clone.docker.stop_container(cid).await;
                        if state_clone.docker.remove_container(cid).await.is_ok()
                            && let Err(e) =
                                status::set_service_removed(&state_clone.db, &project_id_clone, svc_name).await
                        {
                            tracing::warn!(project_id = %project_id_clone, service = %svc_name, error = %e, "compose partial redeploy: failed to clear removed service metadata");
                        }
                    }
                }
            }
        }

        // Start only the targeted services
        if let Err((_, msg)) = crate::routes::manage::start_services(
            &state_clone,
            &project_clone,
            crate::routes::manage::StartServicesOpts {
                force_recreate: true,
                pull_images: false,
                force_pull: false,
                services: Some(target_set),
                connect_orchestrator: true,
                rollback_on_failure: true,
            },
        )
        .await
        {
            anyhow::bail!("start_services failed: {}", msg);
        }
    } else {
        // Full deploy: clean up existing containers, pull images, start all services
        let prefix = format!("litebin-{}.", project_id_clone);
        if let Ok(all_containers) = state_clone.docker.list_containers_by_prefix(&prefix).await {
            for cid in &all_containers {
                let service_name = state_clone
                    .docker
                    .inspect_container(cid)
                    .await
                    .ok()
                    .and_then(|inspect| inspect.name)
                    .and_then(|name| name.trim_start_matches('/').strip_prefix(&prefix).map(str::to_owned));
                let _ = state_clone.docker.stop_container(cid).await;
                if state_clone.docker.remove_container(cid).await.is_ok()
                    && let Some(service_name) = service_name
                    && service_name != litebin_common::types::DOCKER_PROXY_SERVICE
                {
                    let _ = status::set_service_removed(&state_clone.db, &project_id_clone, &service_name).await;
                }
            }
        }

        // Pull all images before starting (fail on any pull error)
        let images: Vec<String> =
            start_order_clone.iter().filter_map(|name| compose_clone.services[name].image.clone()).collect();
        let mut pull_errors = Vec::new();
        for image in &images {
            if !image.starts_with("sha256:") {
                let log_state = state_clone.clone();
                let log_project_id = project_id_clone.clone();
                let on_progress: Box<dyn Fn(&str) + Send + Sync> = Box::new(move |msg: &str| {
                    crate::routes::deploy::logs::push_deploy_log(&log_state, &log_project_id, msg);
                });
                if let Err(e) = state_clone.docker.pull_image_with_progress(image, false, Some(on_progress)).await {
                    pull_errors.push(format!("{}: {}", image, e));
                }
            }
        }
        if !pull_errors.is_empty() {
            let msg = pull_errors.join("; ");
            anyhow::bail!("failed to pull images: {}", msg);
        }

        // Start all services using the unified function
        if let Err((_, msg)) = crate::routes::manage::start_services(
            &state_clone,
            &project_clone,
            crate::routes::manage::StartServicesOpts {
                force_recreate: true,
                pull_images: false, // already pulled above
                force_pull: false,
                services: None,
                connect_orchestrator: true,
                rollback_on_failure: true,
            },
        )
        .await
        {
            anyhow::bail!("start_services failed: {}", msg);
        }
    }

    // Persist node_id for sticky scheduling on redeploys
    if let Err(e) = sqlx::query("UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?")
        .bind(&target_node_id_clone)
        .bind(chrono::Utc::now().timestamp())
        .bind(&project_id_clone)
        .execute(&state_clone.db)
        .await
    {
        tracing::warn!(project_id = %project_id_clone, error = %e, "compose deploy: failed to persist node_id");
    }

    // Full route sync after deploy
    crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Syncing routes...");
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state_clone.config.port);
    let route_entries = crate::routing_helpers::resolve_all_routes(
        &state_clone.db,
        &state_clone.platform.domain(),
        &orchestrator_upstream,
    )
    .await?;
    let _ = state_clone
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
        .await;

    tracing::info!(
        project_id = %project_id_clone,
        services = start_order_clone.len(),
        "compose deploy complete"
    );

    crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Routes synced");
    crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Deployment complete");
    crate::routes::deploy::logs::clear_deploy_logs(&state_clone, &project_id_clone);

    // Trigger route sync for downstream consumers
    let _ = state_clone.route_sync_tx.send(());

    // Clean up old per-service images by digest
    for (svc_name, digest) in &old_service_digests_clone {
        let should_cleanup = target_services_clone.as_ref().is_none_or(|targets| targets.contains(svc_name));
        if should_cleanup {
            crate::routes::manage::cleanup_unused_image(&state_clone, existing_node_id_clone.as_deref(), digest).await;
        }
    }

    Ok(())
}

/// Transition the project to Error after a failed background deploy, honoring
/// partial-redeploy semantics (targeted failures only mark the project row).
pub(super) async fn persist_background_failure(
    state: &AppState,
    project_id: &str,
    target_services: Option<&Vec<String>>,
) {
    let project_error = if target_services.is_some() {
        status::set_project_error_only(&state.db, project_id).await
    } else {
        status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None).await
    };
    if let Err(e) = project_error {
        tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error in background task");
    }
}
