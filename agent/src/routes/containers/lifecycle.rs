use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use litebin_common::types::ProjectStatus;

use crate::AgentState;

use super::env::{env_has_changed, ensure_project_dir_and_env, read_project_env, write_env_snapshot};
use super::metadata::write_project_metadata;
use super::types::{
    CleanupRequest, ErrorResponse, RemoveRequest, RunRequest, RunResponse, StartRequest,
    StartResponse, StopRequest,
};

// ── Single-Container Lifecycle Handlers ──────────────────────────────────────

/// POST /containers/run
/// Pull image and run container. Docker auto-assigns the host port.
/// Returns container_id and mapped_port.
pub async fn run_container(
    State(state): State<AgentState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    // Remove existing container for this project if present (handles redeploy)
    let _ = state.docker.remove_by_name(&req.project_id).await;

    // Pull image before running (skip sha256: — pre-loaded via /images/upload, not from a registry)
    if !req.image.starts_with("sha256:") {
        if let Err(e) = state.docker.pull_image(&req.image).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("failed to pull image: {e}"),
                }),
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
        image: Some(req.image.clone()),
        internal_port: Some(req.internal_port),
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

    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
    match state.docker.run_service_container(&config).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
                tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
            }
            write_env_snapshot(&req.project_id);
            write_project_metadata(&req.project_id, &req.image, req.internal_port, req.cmd.as_deref(), req.memory_limit_mb, req.cpu_limit, req.volumes.clone());
            (StatusCode::OK, Json(RunResponse { container_id, mapped_port })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// POST /containers/recreate
/// Recreate container without pulling the image. Picks up updated .env file.
/// Docker auto-assigns a new host port.
pub async fn recreate_container(
    State(state): State<AgentState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    tracing::info!(project = %req.project_id, image = %req.image, "recreate request received");

    let _ = state.docker.remove_by_name(&req.project_id).await;

    // Ensure the directory and placeholder exist
    ensure_project_dir_and_env(&req.project_id);

    let extra_env = read_project_env(&req.project_id);

    let project = litebin_common::types::Project {
        id: req.project_id.clone(),
        user_id: String::new(),
        name: None,
        description: None,
        image: Some(req.image.clone()),
        internal_port: Some(req.internal_port),
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

    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
    match state.docker.run_service_container(&config).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
                tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
            }
            write_env_snapshot(&req.project_id);
            write_project_metadata(&req.project_id, &req.image, req.internal_port, req.cmd.as_deref(), req.memory_limit_mb, req.cpu_limit, req.volumes.clone());
            (StatusCode::OK, Json(RunResponse { container_id, mapped_port })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// POST /containers/start
/// Start an existing stopped container.
/// If project_id is provided and .env has changed since last injection, recreates instead.
pub async fn start_container(
    State(state): State<AgentState>,
    Json(req): Json<StartRequest>,
) -> impl IntoResponse {
    // Check if .env changed — if so, recreate to pick up new vars
    if let Some(ref project_id) = req.project_id {
        if env_has_changed(project_id) {
            tracing::info!(project = project_id, "env changed since last start, recreating container");

            let image = match &req.image {
                Some(i) => i.clone(),
                None => return (StatusCode::BAD_REQUEST, Json(ErrorResponse {
                    error: "image is required when env has changed and recreate is needed".to_string(),
                })).into_response(),
            };
            let internal_port = match req.internal_port {
                Some(p) => p,
                None => return (StatusCode::BAD_REQUEST, Json(ErrorResponse {
                    error: "internal_port is required when env has changed and recreate is needed".to_string(),
                })).into_response(),
            };

            let _ = state.docker.remove_by_name(project_id).await;
            ensure_project_dir_and_env(project_id);
            let extra_env = read_project_env(project_id);

            let project = litebin_common::types::Project {
                id: project_id.clone(),
                user_id: String::new(),
                name: None,
                description: None,
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
                    write_project_metadata(project_id, &image, internal_port, req.cmd.as_deref(), req.memory_limit_mb, req.cpu_limit, None);
                    (StatusCode::OK, Json(StartResponse { mapped_port })).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: e.to_string() }),
                ).into_response(),
            };
        }
    }

    // Fast path: env unchanged, just start the existing container
    if let Err(e) = state.docker.start_existing_container(&req.container_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response();
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
pub async fn stop_container(
    State(state): State<AgentState>,
    Json(req): Json<StopRequest>,
) -> impl IntoResponse {
    match state.docker.stop_container(&req.container_id).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// POST /containers/remove
pub async fn remove_container(
    State(state): State<AgentState>,
    Json(req): Json<RemoveRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.docker.remove_container(&req.container_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response();
    }

    StatusCode::OK.into_response()
}

/// POST /containers/cleanup
/// Remove all project resources: containers, volumes, network, and project directory.
pub async fn cleanup_project(
    State(state): State<AgentState>,
    Json(req): Json<CleanupRequest>,
) -> impl IntoResponse {
    tracing::info!(project = %req.project_id, "cleanup request received");

    if let Err(e) = state.docker.cleanup_project_resources(&req.project_id, &req.volumes).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response();
    }

    tracing::info!(project = %req.project_id, "cleanup complete");
    StatusCode::OK.into_response()
}
