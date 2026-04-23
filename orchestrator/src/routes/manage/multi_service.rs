use axum::{http::StatusCode, Json};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::AppState;

use super::helpers::{MessageResponse, read_local_project_env, sync_caddy, write_local_env_snapshot};

// ── Multi-Service Helpers ──────────────────────────────────────────────────

/// Stop all service containers for a multi-service project (reverse dependency order).
/// Called from the background task in `stop_project`.
pub async fn stop_all_services(state: &AppState, project_id: &str) {
    let services: Vec<(String, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT service_name, container_id, depends_on FROM project_services WHERE project_id = ? AND status = 'running'",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(project = %project_id, error = %e, "stop: failed to fetch services");
            return;
        }
    };

    // Simple approach: stop in reverse of the fetched order (dependencies first)
    for (svc_name, cid, _) in services.iter().rev() {
        if let Some(container_id) = cid {
            let _ = state.docker.stop_container(container_id).await;
            let _ = sqlx::query(
                "UPDATE project_services SET status = 'stopped', mapped_port = NULL WHERE project_id = ? AND service_name = ?"
            )
            .bind(&project_id)
            .bind(svc_name)
            .execute(&state.db)
            .await;
            tracing::info!(project = %project_id, service = %svc_name, "service stopped");
        }
    }
}

/// Remove all service containers and the per-project network for a multi-service project.
/// Called from `delete_project`.
pub async fn delete_all_services(state: &AppState, project_id: &str) {
    let services: Vec<(String, Option<String>)> = match sqlx::query_as(
        "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project = %project_id, error = %e, "delete: failed to fetch services");
            Vec::new()
        }
    };

    for (svc_name, cid) in &services {
        if let Some(container_id) = cid {
            let _ = state.docker.stop_container(container_id).await;
            let _ = state.docker.remove_container(container_id).await;
            tracing::info!(project = %project_id, service = %svc_name, "service container removed during delete");
        }
    }

    // Remove per-project network
    let _ = state.docker.remove_project_network(&project_id, None).await;
}

/// Result of starting a single service container.
struct StartedService {
    container_id: String,
    mapped_port: u16,
    is_public: bool,
}

/// Start all services for a multi-service project (reads compose.yaml from disk).
/// Uses parallel startup within topological levels and respects depends_on conditions.
/// Used by `start_project` API and can be reused by other callers.
pub async fn start_all_services(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<(), (StatusCode, String)> {
    let project_id = &project.id;

    let compose_yaml = litebin_common::docker::DockerManager::read_compose(project_id)
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "compose.yaml not found".to_string()))?;

    let compose = compose_bollard::ComposeParser::parse(&compose_yaml)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("invalid compose.yaml: {e}")))?;

    // Ensure per-project network
    state.docker.ensure_project_network(project_id, None).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("network error: {e}")))?;

    // Connect Caddy to the project network
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(project_id, None);
    if let Err(e) = state.docker.connect_container_to_network(&caddy_container, &project_network).await {
        tracing::warn!(error = %e, container = %caddy_container, network = %project_network, "failed to connect caddy to project network");
    }

    let extra_env = read_local_project_env(project_id);
    let plan = litebin_common::compose_run::ComposeRunPlan::from_compose(&compose, project_id, &extra_env, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("compose error: {e}")))?;

    // Build owned lookup: service_name -> RunServiceConfig
    // Pre-compute which services need healthcheck waits
    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();
    let healthy_wait_set: std::collections::HashSet<String> = plan.service_order.iter()
        .filter(|s| plan.needs_healthy_wait(s))
        .cloned()
        .collect();
    let has_healthcheck: std::collections::HashSet<String> = plan.service_order.iter()
        .filter(|s| compose.services.get(s.as_str()).and_then(|svc| svc.healthcheck.as_ref()).is_some())
        .cloned()
        .collect();

    let mut public_container_id = String::new();
    let mut public_mapped_port: u16 = 0;

    // Start services level by level — parallel within each level
    for level in &plan.service_levels {
        let mut tasks: JoinSet<Result<StartedService, String>> = JoinSet::new();

        for svc_name in level {
            let run_config = configs_map[svc_name].clone();
            let db = state.db.clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let needs_healthy = healthy_wait_set.contains(svc_name) && has_healthcheck.contains(svc_name);
            let is_public = run_config.is_public;

            tasks.spawn(async move {
                let (container_id, mapped_port) = docker.run_service_container(&run_config).await
                    .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;

                tracing::info!(service = %svc, container_id = %container_id, port = %mapped_port, "multi-service: service started");

                // Wait for network readiness
                if let Err(e) = docker.wait_for_network_ready(&container_id).await {
                    tracing::warn!(service = %svc, error = %e, "network readiness timeout, continuing");
                }

                // Wait for healthcheck only if a downstream service depends on it with service_healthy
                if needs_healthy {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        tracing::warn!(service = %svc, error = %e, "healthcheck failed, continuing");
                    }
                }

                // Update project_services row
                let _ = sqlx::query(
                    "UPDATE project_services SET container_id = ?, mapped_port = ?, status = 'running' WHERE project_id = ? AND service_name = ?"
                )
                .bind(&container_id)
                .bind(mapped_port as i64)
                .bind(&run_config.project_id)
                .bind(&svc)
                .execute(&db)
                .await;

                Ok(StartedService {
                    container_id,
                    mapped_port,
                    is_public,
                })
            });
        }

        // Collect results from this level
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(started)) => {
                    if started.is_public {
                        public_container_id = started.container_id;
                        public_mapped_port = started.mapped_port;
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "multi-service: service failed to start");
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
                }
                Err(e) => {
                    tracing::error!(error = %e, "multi-service: service task panicked");
                }
            }
        }
    }

    write_local_env_snapshot(project_id);
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE projects SET status = 'running', container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(if public_container_id.is_empty() { None } else { Some(public_container_id) })
    .bind(if public_mapped_port == 0 { None } else { Some(public_mapped_port as i64) })
    .bind(now)
    .bind(now)
    .bind(project_id)
    .execute(&state.db)
    .await;

    sync_caddy(state).await;
    tracing::info!(project = %project_id, "all multi-service containers started");
    Ok(())
}

/// Recreate all services for a multi-service project.
/// Stops and removes all existing containers, then re-deploys from compose.yaml.
pub async fn recreate_all_services(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project_id = &project.id;

    // Acquire deploy lock
    let semaphore = state
        .deploy_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // Stop and remove all existing service containers
    let services: Vec<litebin_common::types::ProjectService> = sqlx::query_as(
        "SELECT * FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    for svc in &services {
        if let Some(ref cid) = svc.container_id {
            let _ = state.docker.stop_container(cid).await;
            let _ = state.docker.remove_container(cid).await;
            tracing::info!(project = %project_id, service = %svc.service_name, "recreate: service container removed");
        }
        // Reset service status
        let _ = sqlx::query(
            "UPDATE project_services SET container_id = NULL, mapped_port = NULL, status = 'stopped' WHERE project_id = ? AND service_name = ?"
        )
        .bind(project_id)
        .bind(&svc.service_name)
        .execute(&state.db)
        .await;
    }

    // Clear project-level container cache
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("UPDATE projects SET container_id = NULL, mapped_port = NULL, status = 'stopped', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(project_id)
        .execute(&state.db)
        .await;

    // Re-deploy all services
    start_all_services(state, project).await?;

    Ok(Json(MessageResponse {
        message: format!("project '{}' recreated", project_id),
    }))
}
