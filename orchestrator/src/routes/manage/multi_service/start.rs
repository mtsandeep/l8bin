use std::collections::HashSet;
use std::sync::Arc;

use axum::http::StatusCode;
use tokio::task::JoinSet;

use crate::AppState;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use crate::routes::manage::helpers::{
    local_env_has_changed, read_local_project_env, sync_caddy, write_local_env_snapshot,
};

use super::helpers::{cancellation_cleanup_services, mark_replacement_failure, should_abort_siblings};
use super::opts::StartServicesOpts;

// ── Result of starting a single service ──────────────────────────────────────

struct StartedService {
    service_name: String,
    container_id: String,
    mapped_port: u16,
    is_public: bool,
}

/// Read + parse compose.yaml (or build the single-service plan from the projects
/// row), apply background/public rules, host-network gating, dashboard-set
/// resource overrides, and the allow_raw_ports flag.
async fn build_run_plan(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<(litebin_common::compose_run::ComposeRunPlan, bool), (StatusCode, String)> {
    let project_id = &project.id;

    // 1. Read + parse compose.yaml, or build single-service plan from projects row
    let extra_env = read_local_project_env(project_id);
    let compose_yaml = litebin_common::docker::DockerManager::read_compose(project_id);
    let is_single_image = compose_yaml.is_none();

    let mut plan = if let Some(yaml) = compose_yaml {
        let compose = compose_bollard::ComposeParser::parse_with_interpolation(&yaml, &extra_env, true)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("invalid compose.yaml: {e}")))?;
        litebin_common::compose_run::ComposeRunPlan::from_compose(&compose, project_id, &extra_env, None)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("compose error: {e}")))?
    } else {
        // Single-service fallback: build from projects row (no compose.yaml)
        let config = litebin_common::types::RunServiceConfig::from_project(project, extra_env);
        litebin_common::compose_run::ComposeRunPlan::single_service(config)
    };

    if project.is_background {
        plan.pub_service_name = None;
        for config in &mut plan.configs {
            config.is_public = false;
        }
    }

    let requests_host_network = plan.configs.iter().any(|config| config.host_network);
    if requests_host_network {
        if !project.is_background {
            return Err((StatusCode::BAD_REQUEST, "host networking is restricted to background projects".into()));
        }
        let authorized = crate::capabilities::has_capability(
            &state.db,
            project_id,
            litebin_common::capabilities::ProjectCapability::HostNetwork,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
        if !authorized {
            return Err((StatusCode::FORBIDDEN, "host-network capability was not authorized".into()));
        }
        let host = state.docker.host_info().await.ok();
        litebin_common::docker::require_host_network_eligible(host.as_ref().and_then(|info| info.rootless), Some(3))
            .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    }

    // 1b. Apply per-service overrides from project_services (dashboard-set memory/cpu)
    let db_overrides: Vec<(String, Option<i64>, Option<f64>)> = match sqlx::query_as(
        "SELECT service_name, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "start: failed to fetch service overrides");
            Vec::new()
        }
    };

    for config in &mut plan.configs {
        if let Some((_, mem, cpu)) = db_overrides.iter().find(|(name, _, _)| name == &config.service_name) {
            if mem.is_some() {
                config.memory_limit_mb = *mem;
            }
            if cpu.is_some() {
                config.cpu_limit = *cpu;
            }
        }
    }

    // 1c. Apply allow_raw_ports flag from project settings
    let allow_raw = project.allow_raw_ports;
    for config in &mut plan.configs {
        config.allow_raw_ports = allow_raw;
    }

    Ok((plan, is_single_image))
}

/// Inject/reuse/replace the read-only Docker observation proxy for an explicit
/// normalized grant. Returns (proxy_injected, proxy_created, force_recreate_services).
async fn apply_docker_observe(
    state: &AppState,
    project_id: &str,
    plan: &mut litebin_common::compose_run::ComposeRunPlan,
    opts: &mut StartServicesOpts,
) -> Result<(bool, bool, HashSet<String>), (StatusCode, String)> {
    // 1d. Inject read-only Docker observation only for an explicit normalized grant.
    let docker_observe = crate::capabilities::has_capability(
        &state.db,
        project_id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
    let proxy_injected = if docker_observe {
        plan.inject_docker_observe_proxy(project_id).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to configure Docker observation proxy: {e}"))
        })?
    } else {
        false
    };
    let host_observers = plan.host_docker_observer_names();
    let current_proxy = if proxy_injected {
        state.docker.current_docker_observe_proxy(project_id).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to inspect Docker observation proxy: {e}"))
        })?
    } else {
        None
    };
    let reusable_proxy = current_proxy.as_ref().is_some_and(|(_, port)| host_observers.is_empty() || port.is_some());
    let mut force_recreate_services = HashSet::new();
    if reusable_proxy {
        if let Some((_, Some(port))) = current_proxy {
            plan.inject_host_docker_proxy_endpoint(port);
        }
        plan.reuse_existing_docker_observe_proxy();
        if let Some(ref mut filter) = opts.services {
            filter.remove(litebin_common::types::DOCKER_PROXY_SERVICE);
        }
    } else if proxy_injected {
        force_recreate_services.extend(host_observers.iter().cloned());
        if let Some(ref mut filter) = opts.services {
            plan.expand_for_docker_proxy_replacement(filter);
        }
    } else if opts.services.is_none() {
        state
            .docker
            .remove_by_service_name(project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to clean up previous Docker observation proxy: {e}"),
                )
            })?;
    }
    let proxy_created = proxy_injected && !reusable_proxy;
    if proxy_injected {
        // Pre-pull the proxy image (skip if already local; it's not in project_services so the normal pull logic skips it)
        if proxy_created {
            state.docker.pull_image_with_opts(litebin_common::types::DOCKER_OBSERVE_PROXY_IMAGE, false).await.map_err(
                |e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to prepare Docker observation proxy: {e}")),
            )?;
        }
    }
    Ok((proxy_injected, proxy_created, force_recreate_services))
}

/// Ensure per-project network (+ observation network) and connect Caddy and
/// (optionally) the orchestrator to it.
async fn ensure_networks(
    state: &AppState,
    project_id: &str,
    proxy_injected: bool,
    full_start: bool,
    connect_orchestrator: bool,
) -> Result<(), (StatusCode, String)> {
    // 2. Ensure per-project network + connect Caddy + optionally orchestrator
    state
        .docker
        .ensure_project_network(project_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("network error: {e}")))?;
    if proxy_injected {
        let network = litebin_common::types::docker_observe_network_name(project_id, None);
        state
            .docker
            .ensure_named_network(&network)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Docker observation network error: {e}")))?;
    } else if full_start {
        let network = litebin_common::types::docker_observe_network_name(project_id, None);
        state.docker.remove_named_network(&network).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to remove Docker observation network: {e}"))
        })?;
    }

    let caddy_container = std::env::var("CADDY_CONTAINER_NAME").unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(project_id, None);
    if let Err(e) = state.docker.connect_container_to_network(&caddy_container, &project_network).await {
        tracing::warn!(error = %e, container = %caddy_container, network = %project_network, "failed to connect caddy to project network");
    }

    if connect_orchestrator {
        let orchestrator_container =
            std::env::var("ORCHESTRATOR_CONTAINER_NAME").unwrap_or_else(|_| "litebin-orchestrator".into());
        if let Err(e) = state.docker.connect_container_to_network(&orchestrator_container, &project_network).await {
            tracing::warn!(error = %e, container = %orchestrator_container, network = %project_network, "failed to connect orchestrator to project network");
        }
    }
    Ok(())
}

/// Load existing container IDs per service from the DB (for the fast path).
async fn load_existing_containers(
    db: &sqlx::SqlitePool,
    project_id: &str,
) -> std::collections::HashMap<String, (String, u16)> {
    let rows: Vec<(String, Option<String>, Option<i64>)> = match sqlx::query_as(
        "SELECT service_name, container_id, mapped_port FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "start: failed to fetch existing containers");
            Vec::new()
        }
    };
    let mut map = std::collections::HashMap::new();
    for (name, cid, port) in rows {
        if let Some(cid) = cid {
            map.insert(name, (cid, port.unwrap_or(0) as u16));
        }
    }
    map
}

/// Pull images only for services without existing containers (if requested).
async fn pull_missing_images(
    state: &AppState,
    plan: &litebin_common::compose_run::ComposeRunPlan,
    opts: &StartServicesOpts,
    force_recreate_services: &HashSet<String>,
    existing_containers: &std::collections::HashMap<String, (String, u16)>,
) {
    if opts.pull_images {
        for config in &plan.configs {
            if !config.image.starts_with("sha256:")
                && (opts.force_recreate
                    || force_recreate_services.contains(&config.service_name)
                    || !existing_containers.contains_key(&config.service_name))
            {
                if let Err(e) = state.docker.pull_image_with_opts(&config.image, opts.force_pull).await {
                    tracing::warn!(service = %config.service_name, image = %config.image, error = %e, "pull failed, continuing");
                }
            }
        }
    }
}

// ── Core: start_services ─────────────────────────────────────────────────────

/// Start services for a multi-service project from compose.yaml.
///
/// This is the single source of truth for all multi-service container startup.
/// Callers (waker, dashboard, deploy) pass different opts to get the behavior they need.
pub async fn start_services(
    state: &AppState,
    project: &crate::db::models::Project,
    mut opts: StartServicesOpts,
) -> Result<(), (StatusCode, String)> {
    let project_id = &project.id;

    let (mut plan, is_single_image) = build_run_plan(state, project).await?;
    let (proxy_injected, proxy_created, force_recreate_services) =
        apply_docker_observe(state, project_id, &mut plan, &mut opts).await?;
    ensure_networks(state, project_id, proxy_injected, opts.services.is_none(), opts.connect_orchestrator).await?;

    // 3. Build lookup maps from plan
    let mut configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();
    let healthy_wait_set: HashSet<String> =
        plan.service_order.iter().filter(|s| plan.needs_healthy_wait(s)).cloned().collect();
    let completed_wait_set: HashSet<String> =
        plan.service_order.iter().filter(|s| plan.needs_completed_wait(s)).cloned().collect();
    let has_healthcheck: HashSet<String> = plan
        .service_order
        .iter()
        .filter(|s| {
            plan.configs
                .iter()
                .find(|c| c.service_name == **s)
                .and_then(|c| c.bollard_create_body.as_ref())
                .map(|body| body.healthcheck.is_some())
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    // 4. Pre-load existing containers from DB (for fast-path)
    let existing_containers = load_existing_containers(&state.db, project_id).await;

    // 5. Pull images only for services without existing containers (if requested)
    pull_missing_images(state, &plan, &opts, &force_recreate_services, &existing_containers).await;

    // 6. Start services level by level — parallel within each level
    let mut public_container_id = String::new();
    let mut public_mapped_port: u16 = 0;
    let any_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Track started containers for rollback
    let started_containers: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Track rows whose old container was actually removed. These rows must not
    // retain stale runtime metadata if replacement fails later.
    let removed_services: Arc<std::sync::Mutex<HashSet<String>>> = Arc::new(std::sync::Mutex::new(HashSet::new()));
    // A cancelled Docker create request may still have reached the daemon even
    // though no container ID was returned. Track only services that actually
    // entered a create/recreate call so cancellation cleanup can use names
    // without touching merely inspected pre-existing containers.
    let create_attempted_services: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));

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
            let needs_completed = completed_wait_set.contains(svc_name) || run_config.is_oneshot;
            let is_public = run_config.is_public;
            let is_oneshot = run_config.is_oneshot;
            let existing = existing_containers.get(svc_name).cloned();
            let force_recreate = opts.force_recreate || force_recreate_services.contains(svc_name);
            let any_started = any_started.clone();
            let started_containers = started_containers.clone();
            let removed_services = removed_services.clone();
            let create_attempted_services = create_attempted_services.clone();

            tasks.spawn(async move {
                // One-shot already exited 0: treat as done (Compose behavior)
                if is_oneshot && !force_recreate {
                    if let Some((ref existing_cid, _)) = existing {
                        if !docker.is_container_running(existing_cid).await.unwrap_or(false) {
                            if matches!(docker.container_exit_code(existing_cid).await.ok().flatten(), Some(0)) {
                                if let Err(e) = status::set_service_completed(
                                    &db,
                                    &run_config.project_id,
                                    &svc,
                                    existing_cid,
                                )
                                .await
                                {
                                    tracing::warn!(project_id = %run_config.project_id, service = %svc, error = %e, "start: failed to set service completed");
                                }
                                return Ok(StartedService {
                                    service_name: svc.clone(),
                                    container_id: existing_cid.clone(),
                                    mapped_port: 0,
                                    is_public,
                                });
                            }
                        }
                    }
                }

                let (container_id, mapped_port) = if force_recreate {
                    // Force recreate: always remove + create new
                    if let Some((ref existing_cid, _)) = existing {
                        let _ = docker.stop_container(existing_cid).await;
                        if docker.remove_container(existing_cid).await.is_ok() {
                            if let Ok(mut removed) = removed_services.lock() {
                                removed.insert(svc.clone());
                            }
                        }
                    }
                    if let Ok(mut attempted) = create_attempted_services.lock() {
                        attempted.insert(svc.clone());
                    }
                    let (id, port) = docker.run_service_container(&run_config).await
                        .map_err(|e| format!("failed to create service '{}': {}", svc, e))?;
                    any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                    (id, port)
                } else {
                    // Smart path: try to reuse existing containers
                    if let Some((ref existing_cid, existing_port)) = existing {
                        // Skip reuse if env changed — need recreate to pick up new vars
                        let env_changed = local_env_has_changed(&run_config.project_id);
                        if env_changed {
                            tracing::info!(service = %svc, "env changed, recreating container");
                            if docker.remove_container(existing_cid).await.is_ok() {
                                if let Ok(mut removed) = removed_services.lock() {
                                    removed.insert(svc.clone());
                                }
                            }
                            // fall through to run_service_container below
                        } else if docker.is_container_running(existing_cid).await.unwrap_or(false) {
                            // Already running — fix stale DB status (e.g. stats polling
                            // may have marked it 'stopped' after a transient check failure)
                            if let Err(e) = status::set_service_running(&db, &run_config.project_id, &svc, existing_cid, Some(existing_port as i64)).await {
                                tracing::warn!(project_id = %run_config.project_id, service = %svc, error = %e, "start: failed to set service running (existing container)");
                            }
                            return Ok(StartedService {
                                service_name: svc.clone(),
                                container_id: existing_cid.clone(),
                                mapped_port: existing_port,
                                is_public,
                            });
                        } else if is_oneshot {
                            // Exited one-shot that did not succeed — recreate below
                            if docker.remove_container(existing_cid).await.is_ok() {
                                if let Ok(mut removed) = removed_services.lock() {
                                    removed.insert(svc.clone());
                                }
                            }
                        } else {
                            // Stopped — try docker start (fast path)
                            match docker
                                .start_existing_container(existing_cid, &svc, is_oneshot)
                                .await
                            {
                                Ok(()) => {
                                    any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                                    if let Ok(mut started) = started_containers.lock() {
                                        started.push((svc.clone(), existing_cid.clone()));
                                    }
                                    // Re-resolve mapped port from Docker (may have been cleared on previous stop)
                                    let actual_port = if existing_port == 0 && run_config.is_public {
                                        docker.inspect_mapped_port(existing_cid).await.ok().flatten().unwrap_or(0)
                                    } else {
                                        existing_port
                                    };
                                    // Update service status and mapped port
                                    if let Err(e) = status::set_service_running(&db, &run_config.project_id, &svc, existing_cid, Some(actual_port as i64)).await {
                                        tracing::warn!(project_id = %run_config.project_id, service = %svc, error = %e, "start: failed to set service running (docker start)");
                                    }
                                    tracing::info!(service = %svc, container_id = %existing_cid, "started existing stopped container");
                                    return Ok(StartedService {
                                        service_name: svc.clone(),
                                        container_id: existing_cid.clone(),
                                        mapped_port: actual_port,
                                        is_public,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(service = %svc, error = %e, "docker start failed (stale?), recreating");
                                    // Container is gone or broken — remove stale reference and fall through
                                    if docker.remove_container(existing_cid).await.is_ok() {
                                        if let Ok(mut removed) = removed_services.lock() {
                                            removed.insert(svc.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // No existing container or start failed — create new
                    if let Ok(mut attempted) = create_attempted_services.lock() {
                        attempted.insert(svc.clone());
                    }
                    let (id, port) = docker.run_service_container(&run_config).await
                        .map_err(|e| format!("failed to create service '{}': {}", svc, e))?;
                    any_started.store(true, std::sync::atomic::Ordering::Relaxed);
                    (id, port)
                };

                tracing::info!(service = %svc, container_id = %container_id, port = %mapped_port, "service started");
                if let Ok(mut started) = started_containers.lock() {
                    started.push((svc.clone(), container_id.clone()));
                }

                if svc == litebin_common::types::DOCKER_PROXY_SERVICE {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        let _ = docker.stop_container(&container_id).await;
                        let _ = docker.remove_container(&container_id).await;
                        return Err(format!("Docker observation proxy failed health check: {}", e));
                    }
                }

                // Wait for Docker network to assign a valid IP (skip if container exited)
                if !run_config.host_network && docker.is_container_running(&container_id).await.unwrap_or(false) {
                    if let Err(e) = docker.wait_for_network_ready(&container_id).await {
                        tracing::warn!(service = %svc, error = %e, "network readiness timeout, continuing");
                    }
                }

                // Wait for healthcheck if a downstream service depends on it
                if needs_healthy {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        tracing::warn!(service = %svc, error = %e, "healthcheck failed, continuing");
                    }
                }

                if needs_completed {
                    docker
                        .wait_for_completed_successfully(&container_id)
                        .await
                        .map_err(|e| format!("one-shot service '{}' failed: {}", svc, e))?;
                    if let Err(e) = status::set_service_completed(&db, &run_config.project_id, &svc, &container_id).await {
                        tracing::warn!(project_id = %run_config.project_id, service = %svc, error = %e, "start: failed to set service completed");
                    }
                } else if let Err(e) = status::set_service_running(&db, &run_config.project_id, &svc, &container_id, Some(mapped_port as i64)).await {
                    tracing::warn!(project_id = %run_config.project_id, service = %svc, error = %e, "start: failed to set service running (new container)");
                }

                Ok(StartedService {
                    service_name: svc,
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
                    if started.service_name == litebin_common::types::DOCKER_PROXY_SERVICE
                        && configs_map.values().any(|config| config.host_network && config.docker_observe)
                    {
                        let port = match state.docker.inspect_mapped_port_for(&started.container_id, "2375/tcp").await {
                            Ok(Some(port)) => port,
                            Ok(None) => {
                                tasks.abort_all();
                                while tasks.join_next().await.is_some() {}
                                let started =
                                    started_containers.lock().map(|started| started.clone()).unwrap_or_default();
                                for (_, cid) in started.iter().rev() {
                                    let _ = state.docker.remove_container(cid).await;
                                }
                                let attempted = create_attempted_services
                                    .lock()
                                    .map(|attempted| attempted.clone())
                                    .unwrap_or_default();
                                for service in cancellation_cleanup_services(&attempted, &started) {
                                    let _ = state.docker.remove_by_service_name(project_id, &service, None).await;
                                }
                                let mut affected =
                                    removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                                affected.extend(attempted);
                                affected.extend(started.into_iter().map(|(service, _)| service));
                                mark_replacement_failure(state, project_id, &affected).await;
                                return Err((
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "Docker observation proxy did not receive its required loopback mapping".into(),
                                ));
                            }
                            Err(error) => {
                                tasks.abort_all();
                                while tasks.join_next().await.is_some() {}
                                let started =
                                    started_containers.lock().map(|started| started.clone()).unwrap_or_default();
                                for (_, cid) in started.iter().rev() {
                                    let _ = state.docker.remove_container(cid).await;
                                }
                                let attempted = create_attempted_services
                                    .lock()
                                    .map(|attempted| attempted.clone())
                                    .unwrap_or_default();
                                for service in cancellation_cleanup_services(&attempted, &started) {
                                    let _ = state.docker.remove_by_service_name(project_id, &service, None).await;
                                }
                                let mut affected =
                                    removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                                affected.extend(attempted);
                                affected.extend(started.into_iter().map(|(service, _)| service));
                                mark_replacement_failure(state, project_id, &affected).await;
                                return Err((
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("failed to inspect Docker observation proxy mapping: {error}"),
                                ));
                            }
                        };
                        for config in configs_map.values_mut() {
                            if config.host_network && config.docker_observe {
                                config.env.retain(|value| !value.starts_with("DOCKER_HOST="));
                                config.env.push(format!("DOCKER_HOST=tcp://127.0.0.1:{port}"));
                            }
                        }
                    }
                    if started.is_public {
                        public_container_id = started.container_id;
                        public_mapped_port = started.mapped_port;
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "service failed to start");
                    let cleanup_cancelled = should_abort_siblings(opts.rollback_on_failure, proxy_created);
                    if cleanup_cancelled {
                        tasks.abort_all();
                    }
                    while tasks.join_next().await.is_some() {}
                    if cleanup_cancelled {
                        // Collect container IDs, drop guard, then stop/remove (MutexGuard is not Send)
                        let started = started_containers.lock().map(|s| s.clone()).unwrap_or_default();
                        for (_, cid) in &started {
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                            tracing::warn!(cid = %cid, "rollback: stopped after failure");
                        }
                        let attempted =
                            create_attempted_services.lock().map(|attempted| attempted.clone()).unwrap_or_default();
                        let cleanup_services = cancellation_cleanup_services(&attempted, &started);
                        for service in &cleanup_services {
                            let _ = state.docker.remove_by_service_name(project_id, service, None).await;
                        }
                        let mut affected = removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                        affected.extend(cleanup_services);
                        mark_replacement_failure(state, project_id, &affected).await;
                        let project_error = if opts.services.is_some() {
                            status::set_project_error_only(&state.db, project_id).await
                        } else {
                            status::transition(
                                &state.db,
                                project_id,
                                ProjectStatus::Error,
                                &ProjectUpdateFields::default(),
                                None,
                            )
                            .await
                        };
                        if let Err(e) = project_error {
                            tracing::warn!(project_id = %project_id, error = %e, "start services: failed to transition to Error on rollback");
                        }
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
                    }
                    let affected = removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                    mark_replacement_failure(state, project_id, &affected).await;
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
                }
                Err(e) => {
                    tracing::error!(error = %e, "service task panicked");
                    let cleanup_cancelled = should_abort_siblings(opts.rollback_on_failure, proxy_created);
                    if cleanup_cancelled {
                        tasks.abort_all();
                    }
                    while tasks.join_next().await.is_some() {}
                    if cleanup_cancelled {
                        let started = started_containers.lock().map(|s| s.clone()).unwrap_or_default();
                        for (_, cid) in &started {
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                        }
                        let attempted =
                            create_attempted_services.lock().map(|attempted| attempted.clone()).unwrap_or_default();
                        let cleanup_services = cancellation_cleanup_services(&attempted, &started);
                        for service in &cleanup_services {
                            let _ = state.docker.remove_by_service_name(project_id, service, None).await;
                        }
                        let mut affected = removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                        affected.extend(cleanup_services);
                        mark_replacement_failure(state, project_id, &affected).await;
                    } else {
                        let affected = removed_services.lock().map(|removed| removed.clone()).unwrap_or_default();
                        mark_replacement_failure(state, project_id, &affected).await;
                    }
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, "service task panicked".to_string()));
                }
            }
        }

        // If the Docker observation proxy was started in this level, wait for it
        // to be network-ready before starting the next level. Services that
        // mount docker.sock need the proxy to be accepting connections.
        let proxy_cid = started_containers.lock().ok().and_then(|s| {
            s.iter().find(|(name, _)| name == litebin_common::types::DOCKER_PROXY_SERVICE).map(|(_, cid)| cid.clone())
        });
        if let Some(proxy_cid) = proxy_cid {
            if let Err(e) = state.docker.wait_for_network_ready(&proxy_cid).await {
                tracing::warn!(error = %e, "Docker observation proxy network readiness timeout, continuing");
            } else {
                tracing::info!(container_id = %proxy_cid, "Docker observation proxy is network-ready");
            }
        }
    }

    // 7. Finalize
    write_local_env_snapshot(project_id);
    let now = chrono::Utc::now().timestamp();

    if opts.services.is_none() {
        // Full start: retain denormalized public fields for web projects, then
        // derive the project status from the actual per-service outcomes.
        let single_container_id = if is_single_image {
            started_containers.lock().ok().and_then(|started| {
                started.iter().find(|(name, _)| name == "web").map(|(_, container_id)| container_id.clone())
            })
        } else {
            None
        };
        let persisted_container_id =
            if public_container_id.is_empty() { single_container_id } else { Some(public_container_id) };
        if let Err(e) = sqlx::query(
            "UPDATE projects SET container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(persisted_container_id)
        .bind(if public_mapped_port == 0 { None } else { Some(public_mapped_port as i64) })
        .bind(now)
        .bind(now)
        .bind(project_id)
        .execute(&state.db)
        .await
        {
            tracing::warn!(project_id = %project_id, error = %e, "start services: failed to update project container fields");
        }
        status::derive_and_set_project_status(&state.db, project_id).await;
    } else {
        // Partial start: derive project status from aggregate service states
        if !public_container_id.is_empty() {
            // Update public container info on projects row
            if let Err(e) = sqlx::query("UPDATE projects SET container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?")
                .bind(&public_container_id)
                .bind(public_mapped_port as i64)
                .bind(now)
                .bind(now)
                .bind(project_id)
                .execute(&state.db)
                .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "start services: failed to update public container info");
            }
        }
        status::derive_and_set_project_status(&state.db, project_id).await;
    }

    sync_caddy(state).await;

    // DNS wait if any container was created or re-started (Docker DNS needs time either way)
    if any_started.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    tracing::info!(project = %project_id, "all services started");
    Ok(())
}
