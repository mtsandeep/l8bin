use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use litebin_common::types::ProjectStatus;

use crate::AgentState;

use super::env::{ensure_project_dir_and_env, env_has_changed, read_project_env, write_env_snapshot};
use super::metadata::write_project_metadata;
use super::types::{
    CleanupRequest, ErrorResponse, RemoveRequest, RunRequest, RunResponse, StartRequest, StartResponse,
    StopProjectRequest, StopProjectResponse, StopRequest, StopServiceRequest, StopServiceResponse,
};

// ── Single-Container Lifecycle Handlers ──────────────────────────────────────

fn update_project_meta(state: &AgentState, project_id: &str, is_background: bool, docker_observe: bool) {
    {
        let mut meta = state.project_meta.write().unwrap();
        let entry = meta.entry(project_id.to_string()).or_default();
        entry.is_background = is_background;
        entry.docker_observe = docker_observe;
        if is_background {
            entry.auto_start_enabled = false;
        }
    }
    crate::save_project_meta_to_file(&state.project_meta.read().unwrap());
}

pub(crate) async fn run_single_plan(
    state: &AgentState,
    project: &litebin_common::types::Project,
    extra_env: Vec<String>,
    docker_observe: bool,
) -> anyhow::Result<(String, u16)> {
    let project_id = &project.id;
    state.docker.remove_by_service_name(project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None).await?;

    let config = litebin_common::types::RunServiceConfig::from_project(project, extra_env);
    let mut plan = litebin_common::compose_run::ComposeRunPlan::single_service(config);
    let proxy_injected = if docker_observe { plan.inject_docker_observe_proxy(project_id)? } else { false };

    state.docker.ensure_project_network(project_id, None).await?;
    if proxy_injected {
        state.docker.pull_image_with_opts(litebin_common::types::DOCKER_OBSERVE_PROXY_IMAGE, false).await?;
        let network = litebin_common::types::docker_observe_network_name(project_id, None);
        state.docker.ensure_named_network(&network).await?;
    } else {
        let network = litebin_common::types::docker_observe_network_name(project_id, None);
        let _ = state.docker.remove_named_network(&network).await;
    }

    let mut started = Vec::new();
    let mut workload = None;
    for service_name in &plan.service_order {
        let config = plan
            .configs
            .iter()
            .find(|config| &config.service_name == service_name)
            .ok_or_else(|| anyhow::anyhow!("missing run config for {service_name}"))?;
        match state.docker.run_service_container(config).await {
            Ok((container_id, mapped_port)) => {
                started.push(container_id.clone());
                if config.is_managed_docker_proxy {
                    if let Err(error) = state.docker.wait_for_healthy(&container_id, true).await {
                        for id in started.iter().rev() {
                            let _ = state.docker.stop_container(id).await;
                            let _ = state.docker.remove_container(id).await;
                        }
                        return Err(error);
                    }
                } else {
                    workload = Some((container_id, mapped_port));
                }
            }
            Err(error) => {
                for id in started.iter().rev() {
                    let _ = state.docker.stop_container(id).await;
                    let _ = state.docker.remove_container(id).await;
                }
                return Err(error);
            }
        }
    }

    workload.ok_or_else(|| anyhow::anyhow!("single-image workload was not started"))
}

/// POST /containers/run
/// Pull image and run container. Docker auto-assigns the host port.
/// Returns container_id and mapped_port.
pub async fn run_container(State(state): State<AgentState>, Json(req): Json<RunRequest>) -> impl IntoResponse {
    update_project_meta(&state, &req.project_id, req.internal_port.is_none(), req.docker_observe);
    if req.stage_only {
        ensure_project_dir_and_env(&req.project_id);
        write_project_metadata(
            &req.project_id,
            &req.image,
            req.internal_port,
            req.cmd.as_deref(),
            req.memory_limit_mb,
            req.cpu_limit,
            req.volumes.clone(),
        );
        tracing::info!(project = %req.project_id, "container run staged (no containers started)");
        return (StatusCode::OK, Json(RunResponse { container_id: String::new(), mapped_port: None })).into_response();
    }

    // Remove existing container for this project if present (handles redeploy)
    let _ = state.docker.remove_by_name(&req.project_id).await;

    // Pull image before running (skip sha256: — pre-loaded via /images/upload, not from a registry)
    if !req.image.starts_with("sha256:") {
        if let Err(e) = state.docker.pull_image(&req.image).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to pull image: {e}") }),
            )
                .into_response();
        }
    }

    ensure_project_dir_and_env(&req.project_id);

    let extra_env = read_project_env(&req.project_id);

    let project = litebin_common::types::Project {
        id: req.project_id.clone(),
        user_id: String::new(),
        name: None,
        description: None,
        is_background: req.internal_port.is_none(),
        image: Some(req.image.clone()),
        internal_port: req.internal_port,
        mapped_port: None,
        container_id: None,
        node_id: None,
        status: ProjectStatus::Deploying,
        cmd: req.cmd.clone(),
        memory_limit_mb: req.memory_limit_mb,
        cpu_limit: req.cpu_limit,
        custom_domain: None,
        volumes: req.volumes.as_ref().and_then(|v| litebin_common::types::serialize_volumes(v)),
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        allow_raw_ports: false,
        allow_docker_access: false,
        last_active_at: None,
        service_count: None,
        service_summary: None,
        deploy_type: None,
        created_at: 0,
        updated_at: 0,
    };

    match run_single_plan(&state, &project, extra_env, req.docker_observe).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
                tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
            }
            write_env_snapshot(&req.project_id);
            write_project_metadata(
                &req.project_id,
                &req.image,
                req.internal_port,
                req.cmd.as_deref(),
                req.memory_limit_mb,
                req.cpu_limit,
                req.volumes.clone(),
            );
            (StatusCode::OK, Json(RunResponse { container_id, mapped_port: req.internal_port.map(|_| mapped_port) }))
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response(),
    }
}

/// POST /containers/recreate
/// Recreate container without pulling the image. Picks up updated .env file.
/// Docker auto-assigns a new host port.
pub async fn recreate_container(State(state): State<AgentState>, Json(req): Json<RunRequest>) -> impl IntoResponse {
    tracing::info!(project = %req.project_id, image = %req.image, "recreate request received");
    update_project_meta(&state, &req.project_id, req.internal_port.is_none(), req.docker_observe);

    let _ = state.docker.remove_by_name(&req.project_id).await;

    // Ensure the directory and placeholder exist
    ensure_project_dir_and_env(&req.project_id);

    let extra_env = read_project_env(&req.project_id);

    let project = litebin_common::types::Project {
        id: req.project_id.clone(),
        user_id: String::new(),
        name: None,
        description: None,
        is_background: req.internal_port.is_none(),
        image: Some(req.image.clone()),
        internal_port: req.internal_port,
        mapped_port: None,
        container_id: None,
        node_id: None,
        status: ProjectStatus::Deploying,
        cmd: req.cmd.clone(),
        memory_limit_mb: req.memory_limit_mb,
        cpu_limit: req.cpu_limit,
        custom_domain: None,
        volumes: req.volumes.as_ref().and_then(|v| litebin_common::types::serialize_volumes(v)),
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        allow_raw_ports: false,
        allow_docker_access: false,
        last_active_at: None,
        service_count: None,
        service_summary: None,
        deploy_type: None,
        created_at: 0,
        updated_at: 0,
    };

    match run_single_plan(&state, &project, extra_env, req.docker_observe).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
                tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
            }
            write_env_snapshot(&req.project_id);
            write_project_metadata(
                &req.project_id,
                &req.image,
                req.internal_port,
                req.cmd.as_deref(),
                req.memory_limit_mb,
                req.cpu_limit,
                req.volumes.clone(),
            );
            (StatusCode::OK, Json(RunResponse { container_id, mapped_port: req.internal_port.map(|_| mapped_port) }))
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response(),
    }
}

/// POST /containers/start
/// Start an existing stopped container.
/// If project_id is provided and .env has changed since last injection, recreates instead.
pub async fn start_container(State(state): State<AgentState>, Json(req): Json<StartRequest>) -> impl IntoResponse {
    if state.docker.container_uses_host_network(&req.container_id).await.unwrap_or(false) {
        let persisted_authorized = req.project_id.as_ref().is_some_and(|project_id| {
            state
                .project_meta
                .read()
                .ok()
                .and_then(|meta| meta.get(project_id).cloned())
                .is_some_and(|entry| entry.host_network && entry.is_background)
        });
        if !(persisted_authorized || (req.host_network && req.is_background)) {
            return (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse { error: "host-network workload is not authorized as a background project".into() }),
            )
                .into_response();
        }
        if let Some(project_id) = req.project_id.as_ref() {
            let mut meta = state.project_meta.write().unwrap();
            let entry = meta.entry(project_id.clone()).or_default();
            entry.host_network = true;
            entry.is_background = true;
        }
        let host = state.docker.host_info().await.ok();
        if let Err(error) = litebin_common::docker::require_host_network_eligible(
            host.as_ref().and_then(|info| info.os_type.as_deref()),
            host.as_ref().and_then(|info| info.operating_system.as_deref()),
            host.as_ref().and_then(|info| info.rootless),
            Some(3),
        ) {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(ErrorResponse { error: error.to_string() }))
                .into_response();
        }
    }

    // Check if .env changed — if so, recreate to pick up new vars
    if let Some(ref project_id) = req.project_id {
        if env_has_changed(project_id) {
            tracing::info!(project = project_id, "env changed since last start, recreating container");

            let image = match &req.image {
                Some(i) => i.clone(),
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse {
                            error: "image is required when env has changed and recreate is needed".to_string(),
                        }),
                    )
                        .into_response();
                }
            };
            let internal_port = match req.internal_port {
                Some(p) => p,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse {
                            error: "internal_port is required when env has changed and recreate is needed".to_string(),
                        }),
                    )
                        .into_response();
                }
            };

            let _ = state.docker.remove_by_name(project_id).await;
            ensure_project_dir_and_env(project_id);
            let extra_env = read_project_env(project_id);

            let project = litebin_common::types::Project {
                id: project_id.clone(),
                user_id: String::new(),
                name: None,
                description: None,
                is_background: false,
                image: Some(image.clone()),
                internal_port: Some(internal_port),
                mapped_port: None,
                container_id: None,
                node_id: None,
                status: ProjectStatus::Running,
                cmd: req.cmd.clone(),
                memory_limit_mb: req.memory_limit_mb,
                cpu_limit: req.cpu_limit,
                custom_domain: None,
                volumes: None, // start uses existing container, volumes unchanged
                auto_stop_enabled: false,
                auto_stop_timeout_mins: 0,
                auto_start_enabled: false,
                allow_raw_ports: false,
                allow_docker_access: false,
                last_active_at: None,
                service_count: None,
                service_summary: None,
                deploy_type: None,
                created_at: 0,
                updated_at: 0,
            };

            let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
            return match state.docker.run_service_container(&config).await {
                Ok((_container_id, mapped_port)) => {
                    if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
                        tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
                    }
                    write_env_snapshot(project_id);
                    write_project_metadata(
                        project_id,
                        &image,
                        Some(internal_port),
                        req.cmd.as_deref(),
                        req.memory_limit_mb,
                        req.cpu_limit,
                        None,
                    );
                    (StatusCode::OK, Json(StartResponse { mapped_port })).into_response()
                }
                Err(e) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response()
                }
            };
        }
    }

    // Fast path: env unchanged, just start the existing container
    if let Err(e) = state.docker.start_existing_container(&req.container_id, "web", false).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response();
    }

    // Rebuild agent Caddy config so the new container gets a route
    if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
        tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
    }

    // Inspect to return the mapped port (non-fatal — return 0 if not found)
    let mapped_port = match state.docker.inspect_mapped_port(&req.container_id).await {
        Ok(Some(port)) => port,
        Ok(None) => 0,
        Err(e) => {
            tracing::warn!(container_id = %req.container_id, error = %e, "started container but failed to inspect port");
            0
        }
    };
    (StatusCode::OK, Json(StartResponse { mapped_port })).into_response()
}

/// POST /containers/stop
pub async fn stop_container(State(state): State<AgentState>, Json(req): Json<StopRequest>) -> impl IntoResponse {
    match state.docker.stop_container(&req.container_id).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response(),
    }
}

/// POST /containers/stop-service
/// Stop the current primary container selected by deterministic service identity.
pub async fn stop_service(State(state): State<AgentState>, Json(req): Json<StopServiceRequest>) -> impl IntoResponse {
    if litebin_common::types::primary_service_container_name(&req.project_id, &req.service_name).is_none() {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: "invalid project_id or service_name".into() }))
            .into_response();
    }

    match state.docker.stop_primary_service_container(&req.project_id, &req.service_name).await {
        Ok(stopped) => (StatusCode::OK, Json(StopServiceResponse { stopped })).into_response(),
        Err(error) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: error.to_string() })).into_response()
        }
    }
}

/// POST /containers/stop-project
/// Stop all workloads selected by project identity and remove the managed
/// observation proxy without deleting any other project resources.
pub async fn stop_project(State(state): State<AgentState>, Json(req): Json<StopProjectRequest>) -> impl IntoResponse {
    if req.project_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: "project_id must not be empty".into() }))
            .into_response();
    }

    let mut failures = Vec::new();
    let workload_ids = match state.docker.list_project_workload_containers(&req.project_id).await {
        Ok(ids) => ids,
        Err(error) => {
            failures.push(format!("list workloads: {error}"));
            Vec::new()
        }
    };

    for container_id in &workload_ids {
        if let Err(error) = state.docker.stop_container(container_id).await {
            failures.push(format!("stop container {container_id}: {error}"));
        }
    }

    if let Err(error) =
        state.docker.remove_by_service_name(&req.project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None).await
    {
        failures.push(format!("remove managed observation proxy: {error}"));
    }

    if !failures.is_empty() {
        let shown = failures
            .iter()
            .take(5)
            .map(|failure| failure.chars().take(200).collect::<String>())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!(
                    "project stop failed ({} error{}): {}{}",
                    failures.len(),
                    if failures.len() == 1 { "" } else { "s" },
                    shown,
                    if failures.len() > 5 { "; additional errors omitted" } else { "" }
                ),
            }),
        )
            .into_response();
    }

    (StatusCode::OK, Json(StopProjectResponse { stopped_containers: workload_ids.len() })).into_response()
}

/// POST /containers/remove
pub async fn remove_container(State(state): State<AgentState>, Json(req): Json<RemoveRequest>) -> impl IntoResponse {
    if let Err(e) = state.docker.remove_container(&req.container_id).await {
        if litebin_common::docker::DockerErrorKind::from_anyhow(&e) == litebin_common::docker::DockerErrorKind::NotFound
        {
            return StatusCode::OK.into_response();
        }
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response();
    }

    StatusCode::OK.into_response()
}

/// POST /containers/cleanup
/// Remove all project resources: containers, volumes, network, and project directory.
pub async fn cleanup_project(State(state): State<AgentState>, Json(req): Json<CleanupRequest>) -> impl IntoResponse {
    tracing::info!(project = %req.project_id, "cleanup request received");

    if let Err(e) = state.docker.cleanup_project_resources(&req.project_id, &req.volumes).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response();
    }

    tracing::info!(project = %req.project_id, "cleanup complete");
    StatusCode::OK.into_response()
}
