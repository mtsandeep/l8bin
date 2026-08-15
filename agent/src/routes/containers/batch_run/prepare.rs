use axum::{Json, http::StatusCode, response::IntoResponse};

use crate::AgentState;

use super::super::env::read_project_env;
use super::super::types::ErrorResponse;
use super::rollback::batch_run_error;
use super::types::{BatchRunRequest, host_network_authorized};

/// Parse the compose plan and authorize host networking before any
/// filesystem, metadata, network, or container mutation at the agent trust boundary.
pub(super) async fn analyze_and_authorize(
    state: &AgentState,
    req: &BatchRunRequest,
) -> Result<litebin_common::compose_run::ComposeRunPlan, axum::response::Response> {
    let compatibility = match compose_bollard::analyze_compose_yaml_for_workload(
        &req.compose_yaml,
        None,
        Some(&req.project_id),
        req.is_background,
    ) {
        Ok((_, report)) => report,
        Err(error) => {
            return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: format!("invalid compose: {error}") }))
                .into_response());
        }
    };
    if !compatibility.ok {
        let reasons = compatibility
            .unsupported()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: format!("unsupported compose configuration: {reasons}") }),
        )
            .into_response());
    }
    let extra_env = read_project_env(&req.project_id);
    // `${VAR:?}` is enforced at start, not staging (the node .env isn't complete yet).
    let strict = !req.stage_only;
    let plan = match litebin_common::compose_run::build_compose_run_plan(
        &req.compose_yaml,
        &req.project_id,
        &extra_env,
        None,
        strict,
    ) {
        Ok(plan) => plan,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: format!("invalid compose: {e}") }))
                .into_response());
        }
    };
    let requests_host_network = plan.configs.iter().any(|config| config.host_network);
    if requests_host_network {
        if !req.host_network.unwrap_or(false) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse { error: "host-network capability was not authorized".into() }),
            )
                .into_response());
        }
        if !host_network_authorized(req.host_network.unwrap_or(false), req.is_background) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse { error: "host networking is restricted to background projects".into() }),
            )
                .into_response());
        }
        let host = state.docker.host_info().await.ok();
        if let Err(error) =
            litebin_common::docker::require_host_network_eligible(host.as_ref().and_then(|info| info.rootless), Some(3))
        {
            return Err(
                (StatusCode::UNPROCESSABLE_ENTITY, Json(ErrorResponse { error: error.to_string() })).into_response()
            );
        }
    }
    Ok(plan)
}

/// Ensure the project directory exists, update project metadata, and store compose.yaml.
pub(super) async fn persist_project_files(
    state: &AgentState,
    req: &BatchRunRequest,
) -> Result<(), axum::response::Response> {
    super::super::env::ensure_project_dir_and_env(&req.project_id);
    {
        let mut meta = state.project_meta.write().unwrap();
        let entry = meta.entry(req.project_id.clone()).or_default();
        entry.is_background = req.is_background;
        entry.docker_observe = req.docker_observe.unwrap_or(false);
        entry.host_network = req.host_network.unwrap_or(false);
        if req.is_background {
            entry.auto_start_enabled = false;
        }
    }
    crate::save_project_meta_to_file(&state.project_meta.read().unwrap());

    // Store compose.yaml
    let compose_path = super::super::env::projects_dir().join(&req.project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &req.compose_yaml) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to store compose.yaml: {e}") }),
        )
            .into_response());
    }
    Ok(())
}

/// Plan mutations decided for this run: observation flag, whether the proxy
/// was actually injected into the plan, and the resolved target set.
pub(super) struct PlanMutations {
    pub docker_observe: bool,
    pub proxy_injected: bool,
    pub target_set: Option<std::collections::HashSet<String>>,
}

/// Apply background/public rules, raw-ports, the docker-observe proxy
/// lifecycle (inject/reuse/replace), resource overrides, and global defaults.
pub(super) async fn mutate_plan(
    state: &AgentState,
    req: &BatchRunRequest,
    plan: &mut litebin_common::compose_run::ComposeRunPlan,
) -> Result<PlanMutations, axum::response::Response> {
    if req.is_background {
        plan.pub_service_name = None;
        for config in plan.configs.iter_mut() {
            config.is_public = false;
        }
    }

    // Apply allow_raw_ports flag from orchestrator
    if req.allow_raw_ports.unwrap_or(false) {
        for config in plan.configs.iter_mut() {
            config.allow_raw_ports = true;
        }
    }

    // Inject the read-only observation proxy only for the explicit new capability.
    let docker_observe = req.docker_observe.unwrap_or(false);
    let proxy_injected = if docker_observe {
        match plan.inject_docker_observe_proxy(&req.project_id) {
            Ok(injected) => injected,
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("failed to configure Docker observation proxy: {e}") }),
                )
                    .into_response());
            }
        }
    } else {
        false
    };
    let mut target_set: Option<std::collections::HashSet<String>> =
        req.target_services.as_ref().map(|services| services.iter().cloned().collect());
    let host_observers = plan.host_docker_observer_names();
    let current_proxy = if proxy_injected {
        match state.docker.current_docker_observe_proxy(&req.project_id).await {
            Ok(proxy) => proxy,
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("failed to inspect Docker observation proxy: {e}") }),
                )
                    .into_response());
            }
        }
    } else {
        None
    };
    // Reuse is deliberately limited to partial operations. A full start/deploy
    // recreates host observers so no stopped container can retain stale env.
    let reusable_proxy = target_set.is_some()
        && current_proxy.as_ref().is_some_and(|(_, port)| host_observers.is_empty() || port.is_some());
    if reusable_proxy {
        if let Some((_, Some(port))) = current_proxy {
            plan.inject_host_docker_proxy_endpoint(port);
        }
        plan.reuse_existing_docker_observe_proxy();
        if let Some(ref mut targets) = target_set {
            targets.remove(litebin_common::types::DOCKER_PROXY_SERVICE);
        }
    } else if proxy_injected {
        if let Some(ref mut targets) = target_set {
            plan.expand_for_docker_proxy_replacement(targets);
        }
    } else if target_set.is_none() {
        if let Err(e) = state
            .docker
            .remove_by_service_name(&req.project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None)
            .await
        {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to clean up previous Docker observation proxy: {e}") }),
            )
                .into_response());
        }
    }
    let proxy_created = proxy_injected && !reusable_proxy;
    if proxy_created {
        if let Err(e) =
            state.docker.pull_image_with_opts(litebin_common::types::DOCKER_OBSERVE_PROXY_IMAGE, false).await
        {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to prepare Docker observation proxy: {e}") }),
            )
                .into_response());
        }
    } else if !proxy_injected && target_set.is_none() {
        let network = litebin_common::types::docker_observe_network_name(&req.project_id, None);
        if let Err(e) = state.docker.remove_named_network(&network).await {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to remove Docker observation network: {e}") }),
            )
                .into_response());
        }
    }

    // Apply per-service resource overrides from orchestrator (dashboard-set memory/CPU)
    if let Some(ref overrides) = req.service_resources {
        for config in plan.configs.iter_mut() {
            if let Some(res) = overrides.get(&config.service_name) {
                if res.memory_limit_mb.is_some() {
                    config.memory_limit_mb = res.memory_limit_mb;
                }
                if res.cpu_limit.is_some() {
                    config.cpu_limit = res.cpu_limit;
                }
            }
        }
    }

    // Apply global defaults for services that still have no explicit limit
    if req.default_memory_limit_mb.is_some() || req.default_cpu_limit.is_some() {
        for config in plan.configs.iter_mut() {
            if config.memory_limit_mb.is_none() {
                config.memory_limit_mb = req.default_memory_limit_mb;
            }
            if config.cpu_limit.is_none() {
                config.cpu_limit = req.default_cpu_limit;
            }
        }
    }

    Ok(PlanMutations { docker_observe, proxy_injected, target_set })
}

/// Remove existing project containers (targeted on partial redeploy), ensure
/// networks, connect the agent Caddy, and pull images in parallel.
/// Returns the removed service names (for failure metadata).
pub(super) async fn cleanup_and_prepare(
    state: &AgentState,
    req: &BatchRunRequest,
    plan: &litebin_common::compose_run::ComposeRunPlan,
    mutations: &PlanMutations,
    proxy_injected: bool,
) -> Result<Vec<String>, axum::response::Response> {
    // Clean up existing containers from a previous deploy (by name prefix)
    // On partial redeploy, only remove targeted service containers
    let prefix = format!("litebin-{}.", req.project_id);
    let all_containers = match state.docker.list_containers_by_prefix(&prefix).await {
        Ok(containers) => containers,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to list existing project containers: {e}") }),
            )
                .into_response());
        }
    };
    let mut removed_services = Vec::new();
    for cid in &all_containers {
        let inspect = match state.docker.inspect_container(cid).await {
            Ok(inspect) => inspect,
            Err(e) => {
                return Err(batch_run_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to inspect existing project container: {e}"),
                    &removed_services,
                ));
            }
        };
        let service_name = inspect
            .name
            .as_deref()
            .map(|name| name.trim_start_matches('/'))
            .and_then(|name| name.strip_prefix(&prefix))
            .map(str::to_owned);
        let should_remove = mutations
            .target_set
            .as_ref()
            .is_none_or(|targets| service_name.as_ref().is_some_and(|service| targets.contains(service)));
        if should_remove {
            if let Err(e) = state.docker.remove_container(cid).await {
                return Err(batch_run_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to remove existing project container: {e}"),
                    &removed_services,
                ));
            }
            if let Some(service_name) = service_name {
                removed_services.push(service_name);
            }
        }
    }

    // Ensure per-project network
    if let Err(e) = state.docker.ensure_project_network(&req.project_id, None).await {
        return Err(batch_run_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create project network: {e}"),
            &removed_services,
        ));
    }
    if proxy_injected {
        let network = litebin_common::types::docker_observe_network_name(&req.project_id, None);
        if let Err(e) = state.docker.ensure_named_network(&network).await {
            return Err(batch_run_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create Docker observation network: {e}"),
                &removed_services,
            ));
        }
    }

    // Connect the AGENT's Caddy to the project network so it can proxy to containers.
    let caddy_container = litebin_common::types::agent_caddy_container_name();
    let project_network = litebin_common::types::project_network_name(&req.project_id, None);
    let _ = state.docker.connect_container_to_network(&caddy_container, &project_network).await;

    // Pull images in parallel (only for target services on partial redeploy)
    // Skip sha256: images — they were pre-loaded via /images/upload, not from a registry.
    let images_to_pull: Vec<String> = if let Some(ref targets) = mutations.target_set {
        plan.configs.iter().filter(|c| targets.contains(&c.service_name)).map(|c| c.image.clone()).collect()
    } else {
        plan.configs.iter().map(|c| c.image.clone()).collect()
    };
    let images_to_pull: Vec<String> = images_to_pull.into_iter().filter(|img| !img.starts_with("sha256:")).collect();
    let force_pull = req.force_pull;
    let pull_handles: Vec<_> = images_to_pull
        .into_iter()
        .map(|image| {
            let docker = state.docker.clone();
            tokio::spawn(async move {
                (image.clone(), docker.pull_image_with_opts(&image, force_pull).await.map_err(|e| e.to_string()))
            })
        })
        .collect();

    for handle in pull_handles {
        if let Ok((image, result)) = handle.await {
            if let Err(e) = result {
                tracing::error!(image = %image, error = %e, "batch-run: failed to pull image");
            }
        }
    }

    Ok(removed_services)
}
