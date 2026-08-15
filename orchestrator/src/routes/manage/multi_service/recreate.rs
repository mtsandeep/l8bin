use std::collections::HashSet;
use std::sync::Arc;

use axum::{Json, http::StatusCode};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::status;

use crate::routes::manage::helpers::MessageResponse;
use crate::routes::manage::{capture_service_digests, cleanup_unused_image};

use super::opts::StartServicesOpts;
use super::start::start_services;

// ── Recreate services ─────────────────────────────────────────────────────────

/// Recreate services for a multi-service project.
/// If `target_services` is None, recreates all services.
/// If `target_services` is Some, recreates only the listed services.
/// If `pull_images` is true, pulls latest images before recreating (redeploy).
pub async fn recreate_services(
    state: &AppState,
    project: &crate::db::models::Project,
    target_services: Option<Vec<String>>,
    pull_images: bool,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project_id = &project.id;

    // Acquire project lock
    let semaphore =
        state.project_locks.entry(project_id.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let _permit = semaphore.acquire().await.unwrap();

    let target_set: Option<HashSet<String>> = target_services.map(|v| v.into_iter().collect());
    let service_count = target_set.as_ref().map(|s| s.len()).unwrap_or(0);

    // Capture old image digests before stopping containers (for cleanup after recreate with pull)
    let old_digests: std::collections::HashMap<String, String> = if pull_images {
        let node_id = project.node_id.as_deref().unwrap_or("local");
        capture_service_digests(state, project_id, Some(node_id), target_set.as_ref()).await
    } else {
        std::collections::HashMap::new()
    };

    // Stop and remove targeted service containers
    let services: Vec<litebin_common::types::ProjectService> =
        sqlx::query_as("SELECT * FROM project_services WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    for svc in &services {
        if let Some(ref filter) = target_set
            && !filter.contains(&svc.service_name)
        {
            continue;
        }
        if let Some(ref cid) = svc.container_id {
            let _ = state.docker.stop_container(cid).await;
            if state.docker.remove_container(cid).await.is_ok() {
                if let Err(e) = status::set_service_removed(&state.db, project_id, &svc.service_name).await {
                    tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "recreate: failed to clear removed service metadata");
                }
                tracing::info!(project = %project_id, service = %svc.service_name, "recreate: service container removed");
                continue;
            }
        }
        if let Err(e) = status::set_service_stopped(&state.db, project_id, &svc.service_name).await {
            tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "recreate: failed to set service stopped");
        }
    }

    // Re-deploy targeted services
    start_services(
        state,
        project,
        StartServicesOpts {
            force_recreate: true,
            pull_images,
            force_pull: pull_images,
            services: target_set,
            connect_orchestrator: true,
            rollback_on_failure: false,
        },
    )
    .await?;

    // Clean up old images by digest after successful recreate with pull
    if !old_digests.is_empty() {
        let node_id = project.node_id.as_deref().unwrap_or("local");
        for digest in old_digests.values() {
            cleanup_unused_image(state, Some(node_id), digest).await;
        }
    }

    let count = if service_count > 0 { service_count } else { services.len() };
    let action = if pull_images { "redeployed" } else { "recreated" };

    let docker_observe = crate::capabilities::has_capability(
        &state.db,
        project_id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    .unwrap_or(false);
    let warnings = if !docker_observe {
        let compose_path = std::path::PathBuf::from("projects").join(project_id).join("compose.yaml");
        let compose_yaml = std::fs::read_to_string(compose_path).unwrap_or_default();
        if compose_yaml.contains("/docker.sock") {
            vec!["Docker socket declaration found without docker-observe — the raw socket was removed".into()]
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Ok(Json(MessageResponse {
        message: format!("{} service(s) {} for project '{}'", count, action, project_id),
        warnings,
    }))
}
