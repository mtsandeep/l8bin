use axum::{http::StatusCode, Json};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::AppState;
use crate::status::{self, ProjectUpdateFields};

use super::helpers::{MessageResponse, read_local_project_env, sync_caddy, write_local_env_snapshot};

// ── Options ──────────────────────────────────────────────────────────────────

/// Options that control how `start_services` behaves.
pub struct StartServicesOpts {
    /// Always remove and recreate containers (skip fast-path docker start).
    pub force_recreate: bool,

    /// Pull images before starting (for fresh deploys).
    pub pull_images: bool,

    /// Only start these services. None = all services.
    pub services: Option<HashSet<String>>,

    /// Connect the orchestrator container to the project network (needed for proxy).
    pub connect_orchestrator: bool,

    /// On failure, stop and remove all containers started in this call.
    pub rollback_on_failure: bool,
}

impl Default for StartServicesOpts {
    fn default() -> Self {
        Self {
            force_recreate: false,
            pull_images: false,
            services: None,
            connect_orchestrator: false,
            rollback_on_failure: false,
        }
    }
}

// ── Result of starting a single service ──────────────────────────────────────

struct StartedService {
    container_id: String,
    mapped_port: u16,
    is_public: bool,
}

// ── Core: start_services ─────────────────────────────────────────────────────

/// Start services for a multi-service project from compose.yaml.
///
/// This is the single source of truth for all multi-service container startup.
/// Callers (waker, dashboard, deploy) pass different opts to get the behavior they need.
pub async fn start_services(
    state: &AppState,
    project: &crate::db::models::Project,
    opts: StartServicesOpts,
) -> Result<(), (StatusCode, String)> {
    let project_id = &project.id;

    // 1. Read + parse compose.yaml with variable interpolation
    let compose_yaml = litebin_common::docker::DockerManager::read_compose(project_id)
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "compose.yaml not found".to_string()))?;

    let extra_env = read_local_project_env(project_id);
    let compose = compose_bollard::ComposeParser::parse_with_interpolation(&compose_yaml, &extra_env)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("invalid compose.yaml: {e}")))?;

    // 2. Ensure per-project network + connect Caddy + optionally orchestrator
    state.docker.ensure_project_network(project_id, None).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("network error: {e}")))?;

    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(project_id, None);
    if let Err(e) = state.docker.connect_container_to_network(&caddy_container, &project_network).await {
        tracing::warn!(error = %e, container = %caddy_container, network = %project_network, "failed to connect caddy to project network");
    }

    if opts.connect_orchestrator {
        let orchestrator_container = std::env::var("ORCHESTRATOR_CONTAINER_NAME")
            .unwrap_or_else(|_| "litebin-orchestrator".into());
        if let Err(e) = state.docker.connect_container_to_network(&orchestrator_container, &project_network).await {
            tracing::warn!(error = %e, container = %orchestrator_container, network = %project_network, "failed to connect orchestrator to project network");
        }
    }

    // 3. Build plan + lookup maps
    let plan = litebin_common::compose_run::ComposeRunPlan::from_compose(&compose, project_id, &extra_env, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("compose error: {e}")))?;

    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();
    let healthy_wait_set: HashSet<String> = plan.service_order.iter()
        .filter(|s| plan.needs_healthy_wait(s))
        .cloned()
        .collect();
    let has_healthcheck: HashSet<String> = plan.service_order.iter()
        .filter(|s| compose.services.get(s.as_str()).and_then(|svc| svc.healthcheck.as_ref()).is_some())
        .cloned()
        .collect();

    // 4. Pre-load existing containers from DB (for fast-path)
    let existing_containers: std::collections::HashMap<String, (String, u16)> = {
        let rows: Vec<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT service_name, container_id, mapped_port FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let mut map = std::collections::HashMap::new();
        for (name, cid, port) in rows {
            if let Some(cid) = cid {
                map.insert(name, (cid, port.unwrap_or(0) as u16));
            }
        }
        map
    };

    // 5. Pull images only for services without existing containers (if requested)
    if opts.pull_images {
        for config in &plan.configs {
            if !config.image.starts_with("sha256:") && (opts.force_recreate || !existing_containers.contains_key(&config.service_name)) {
                if let Err(e) = state.docker.pull_image(&config.image).await {
                    tracing::warn!(service = %config.service_name, image = %config.image, error = %e, "pull failed, continuing");
                }
            }
        }
    }

    // 6. Start services level by level — parallel within each level
    let mut public_container_id = String::new();
    let mut public_mapped_port: u16 = 0;
    let any_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Track started containers for rollback
    let started_containers: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    for level in &plan.service_levels {
        let mut tasks: JoinSet<Result<StartedService, String>> = JoinSet::new();

        for svc_name in level {
            // Apply service filter
            if let Some(ref filter) = opts.services {
                if !filter.contains(svc_name) {
                    continue;
                }
            }

            let run_config = configs_map[svc_name].clone();
            let db = state.db.clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let needs_healthy = healthy_wait_set.contains(svc_name) && has_healthcheck.contains(svc_name);
            let is_public = run_config.is_public;
            let existing = existing_containers.get(svc_name).cloned();
            let any_started = any_started.clone();
            let started_containers = started_containers.clone();

            tasks.spawn(async move {
                let (container_id, mapped_port) = if opts.force_recreate {
                    // Force recreate: always remove + create new
                    if let Some((ref existing_cid, _)) = existing {
                        let _ = docker.stop_container(existing_cid).await;
                        let _ = docker.remove_container(existing_cid).await;
                    }
                    let (id, port) = docker.run_service_container(&run_config).await
                        .map_err(|e| format!("failed to create service '{}': {}", svc, e))?;
                    any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                    (id, port)
                } else {
                    // Smart path: try to reuse existing containers
                    if let Some((ref existing_cid, existing_port)) = existing {
                        if docker.is_container_running(existing_cid).await.unwrap_or(false) {
                            // Already running — fix stale DB status (e.g. stats polling
                            // may have marked it 'stopped' after a transient check failure)
                            let _ = status::set_service_running(&db, &run_config.project_id, &svc, existing_cid, Some(existing_port as i64)).await;
                            return Ok(StartedService {
                                container_id: existing_cid.clone(),
                                mapped_port: existing_port,
                                is_public,
                            });
                        }
                        // Stopped — try docker start (fast path)
                        match docker.start_existing_container(existing_cid).await {
                            Ok(()) => {
                                any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                                // Re-resolve mapped port from Docker (may have been cleared on previous stop)
                                let actual_port = if existing_port == 0 && run_config.is_public {
                                    docker.inspect_mapped_port(existing_cid).await.unwrap_or(0)
                                } else {
                                    existing_port
                                };
                                // Update service status and mapped port
                                let _ = status::set_service_running(&db, &run_config.project_id, &svc, existing_cid, Some(actual_port as i64)).await;
                                tracing::info!(service = %svc, container_id = %existing_cid, "started existing stopped container");
                                return Ok(StartedService {
                                    container_id: existing_cid.clone(),
                                    mapped_port: actual_port,
                                    is_public,
                                });
                            }
                            Err(e) => {
                                tracing::warn!(service = %svc, error = %e, "docker start failed (stale?), recreating");
                                // Container is gone or broken — remove stale reference and fall through
                                let _ = docker.remove_container(existing_cid).await;
                            }
                        }
                    }
                    // No existing container or start failed — create new
                    let (id, port) = docker.run_service_container(&run_config).await
                        .map_err(|e| format!("failed to create service '{}': {}", svc, e))?;
                    any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                    (id, port)
                };

                tracing::info!(service = %svc, container_id = %container_id, port = %mapped_port, "service started");

                // Wait for Docker network to assign a valid IP
                if let Err(e) = docker.wait_for_network_ready(&container_id).await {
                    tracing::warn!(service = %svc, error = %e, "network readiness timeout, continuing");
                }

                // Wait for healthcheck if a downstream service depends on it
                if needs_healthy {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        tracing::warn!(service = %svc, error = %e, "healthcheck failed, continuing");
                    }
                }

                // Update project_services row
                let _ = status::set_service_running(&db, &run_config.project_id, &svc, &container_id, Some(mapped_port as i64)).await;

                // Track for rollback
                if let Ok(mut started) = started_containers.lock() {
                    started.push((svc, container_id.clone()));
                }

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
                    tracing::error!(error = %e, "service failed to start");
                    if opts.rollback_on_failure {
                        // Collect container IDs, drop guard, then stop/remove (MutexGuard is not Send)
                        let cids: Vec<String> = started_containers.lock()
                            .map(|s| s.iter().map(|(_, cid)| cid.clone()).collect())
                            .unwrap_or_default();
                        for cid in &cids {
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                            tracing::warn!(cid = %cid, "rollback: stopped after failure");
                        }
                        // Reset all service statuses
                        let svc_names: Vec<String> = started_containers.lock()
                            .map(|s| s.iter().map(|(name, _)| name.clone()).collect())
                            .unwrap_or_default();
                        for svc_name in &svc_names {
                            let _ = status::set_service_stopped(&state.db, project_id, svc_name).await;
                        }
                        let _ = status::transition(&state.db, project_id, "error", &ProjectUpdateFields::default(), None).await;
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
                    }
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
                }
                Err(e) => {
                    tracing::error!(error = %e, "service task panicked");
                    if opts.rollback_on_failure {
                        let cids: Vec<String> = started_containers.lock()
                            .map(|s| s.iter().map(|(_, cid)| cid.clone()).collect())
                            .unwrap_or_default();
                        for cid in &cids {
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                        }
                    }
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, "service task panicked".to_string()));
                }
            }
        }
    }

    // 7. Finalize
    write_local_env_snapshot(project_id);
    let now = chrono::Utc::now().timestamp();

    // Only update projects row if we started a public service or all services.
    // When starting a specific non-public service, preserve the existing public container info.
    if !public_container_id.is_empty() || opts.services.is_none() {
        let _ = status::transition(&state.db, project_id, "running", &ProjectUpdateFields {
            container_id: Some(if public_container_id.is_empty() { None } else { Some(public_container_id) }),
            mapped_port: Some(if public_mapped_port == 0 { None } else { Some(public_mapped_port as i64) }),
            last_active_at: Some(now),
            ..Default::default()
        }, None).await;
    } else {
        // Just update status and timestamp, preserve existing container info
        let _ = status::transition(&state.db, project_id, "running", &ProjectUpdateFields {
            last_active_at: Some(now),
            ..Default::default()
        }, None).await;
    }

    sync_caddy(state).await;

    // DNS wait if any container was created or re-started (Docker DNS needs time either way)
    if any_started.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    tracing::info!(project = %project_id, "all services started");
    Ok(())
}

// ── Stop services ─────────────────────────────────────────────────────────────

/// Stop service containers for a multi-service project.
/// If `services` is None, stops all running services.
/// If `services` is Some, stops only the listed services.
/// Updates `project_services` status internally. Caller handles `projects` table and sync_caddy.
pub async fn stop_services(
    state: &AppState,
    project_id: &str,
    services: Option<&HashSet<String>>,
) {
    let rows: Vec<(String, Option<String>)> = match sqlx::query_as(
        "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND status IN ('running', 'stopping')",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(project = %project_id, error = %e, "stop: failed to fetch services");
            return;
        }
    };

    for (svc_name, cid) in rows.iter().rev() {
        // Apply service filter
        if let Some(filter) = services {
            if !filter.contains(svc_name) {
                continue;
            }
        }
        if let Some(container_id) = cid {
            let _ = state.docker.stop_container(container_id).await;
            let _ = status::set_service_stopped(&state.db, project_id, svc_name).await;
            tracing::info!(project = %project_id, service = %svc_name, "service stopped");
        }
    }
}

/// Stop all service containers for a multi-service project.
/// Thin wrapper around `stop_services` for backward compatibility.
pub async fn stop_all_services(state: &AppState, project_id: &str) {
    stop_services(state, project_id, None).await;
}

// ── Delete services ──────────────────────────────────────────────────────────

/// Remove all service containers, volumes, and the per-project network for a multi-service project.
/// Called from `delete_project`.
pub async fn delete_all_services(state: &AppState, project_id: &str) {
    // Fetch volume names from DB
    let volumes: Vec<String> = match sqlx::query_as::<_, (String,)>(
        "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v.into_iter().map(|(name,)| name).collect(),
        Err(e) => {
            tracing::warn!(project = %project_id, error = %e, "delete: failed to fetch volumes");
            Vec::new()
        }
    };

    let _ = state.docker.cleanup_project_resources(project_id, &volumes).await;
}

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
    let semaphore = state
        .project_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    let target_set: Option<HashSet<String>> = target_services.map(|v| v.into_iter().collect());
    let service_count = target_set.as_ref().map(|s| s.len()).unwrap_or(0);

    // Stop and remove targeted service containers
    let services: Vec<litebin_common::types::ProjectService> = sqlx::query_as(
        "SELECT * FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    for svc in &services {
        if let Some(ref filter) = target_set {
            if !filter.contains(&svc.service_name) {
                continue;
            }
        }
        if let Some(ref cid) = svc.container_id {
            let _ = state.docker.stop_container(cid).await;
            let _ = state.docker.remove_container(cid).await;
            tracing::info!(project = %project_id, service = %svc.service_name, "recreate: service container removed");
        }
        let _ = status::set_service_stopped(&state.db, project_id, &svc.service_name).await;
    }

    // Re-deploy targeted services
    start_services(state, project, StartServicesOpts {
        force_recreate: true,
        pull_images,
        services: target_set,
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await?;

    let count = if service_count > 0 { service_count } else { services.len() };
    let action = if pull_images { "redeployed" } else { "recreated" };
    Ok(Json(MessageResponse {
        message: format!("{} service(s) {} for project '{}'", count, action, project_id),
    }))
}
