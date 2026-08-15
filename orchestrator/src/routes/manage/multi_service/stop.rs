use std::collections::HashSet;

use axum::http::StatusCode;

use crate::AppState;
use crate::status;

use super::helpers::{approved_docker_observe_requesters, proxy_needed_after_stop};

// ── Stop services ─────────────────────────────────────────────────────────────

/// Stop service containers for a multi-service project.
/// If `services` is None, stops all running services.
/// If `services` is Some, stops only the listed services.
/// Updates `project_services` status internally. Caller handles `projects` table and sync_caddy.
pub async fn stop_services(
    state: &AppState,
    project_id: &str,
    services: Option<&HashSet<String>>,
) -> Result<(), (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to fetch project: {e}")))?;
    let requesters = approved_docker_observe_requesters(state, &project).await?;
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND status IN ('running', 'stopping')",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to fetch services: {e}")))?;
    let running_services: HashSet<String> = rows.iter().map(|(service, _)| service.clone()).collect();

    for (svc_name, cid) in rows.iter().rev() {
        // Apply service filter
        if let Some(filter) = services
            && !filter.contains(svc_name)
        {
            continue;
        }
        if let Some(container_id) = cid {
            state.docker.stop_container(container_id).await.map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to stop service '{svc_name}': {e}"))
            })?;
            tracing::info!(project = %project_id, service = %svc_name, "service stopped");
        }
        status::set_service_stopped(&state.db, project_id, svc_name).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to persist stopped service '{svc_name}': {e}"))
        })?;
    }

    // Persist the aggregate workload outcome before best-effort infrastructure
    // cleanup so a proxy removal failure cannot leave the project falsely running.
    status::derive_and_set_project_status(&state.db, project_id).await;

    if !proxy_needed_after_stop(&requesters, &running_services, services) {
        state
            .docker
            .remove_by_service_name(project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None)
            .await
            .map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to remove Docker observation proxy: {e}"))
            })?;
        tracing::info!(project = %project_id, "Docker observation proxy removed");
    }
    Ok(())
}
