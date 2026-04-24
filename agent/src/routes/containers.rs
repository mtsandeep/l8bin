use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use litebin_common::types::{ContainerStatus, VolumeMount};
use serde::{Deserialize, Serialize};
use std::hash::{Hash as StdHash, Hasher};
use std::collections::hash_map::DefaultHasher;
use tokio::task::JoinSet;
use crate::AgentState;

fn projects_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("projects")
}

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RunRequest {
    pub image: String,
    pub internal_port: i64,
    pub project_id: String,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub volumes: Option<Vec<VolumeMount>>,
}

#[derive(Serialize)]
pub struct RunResponse {
    pub container_id: String,
    pub mapped_port: u16,
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub container_id: String,
    pub project_id: Option<String>,
    pub image: Option<String>,
    pub internal_port: Option<i64>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
}

#[derive(Serialize)]
pub struct StartResponse {
    pub mapped_port: u16,
}

#[derive(Deserialize)]
pub struct StopRequest {
    pub container_id: String,
}

#[derive(Deserialize)]
pub struct RemoveRequest {
    pub container_id: String,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub tail: Option<usize>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Ensure the project-specific directory exists and has a placeholder .env if missing.
fn ensure_project_dir_and_env(project_id: &str) {
    let project_dir = projects_dir().join(project_id);
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        tracing::error!(project = project_id, error = %e, "failed to create project directory");
        return;
    }

    let env_path = project_dir.join(".env");
    if !env_path.exists() {
        let placeholder = "# Place your runtime environment variables here\n# These variables are injected directly into your container at startup.\n";
        if let Err(e) = std::fs::write(&env_path, placeholder) {
            tracing::error!(project = project_id, error = %e, "failed to create placeholder .env");
        } else {
            tracing::info!(project = project_id, path = %env_path.display(), "created placeholder .env");
        }
    }
}

/// Read env vars from `projects/<project_id>/.env` if it exists.
pub fn read_project_env(project_id: &str) -> Vec<String> {
    // First, ensure the directory and placeholder exist
    ensure_project_dir_and_env(project_id);

    let env_path = projects_dir().join(project_id).join(".env");
    tracing::info!(project = project_id, path = %env_path.display(), exists = env_path.exists(), "checking for .env file");

    if !env_path.exists() {
        return Vec::new();
    }

    match dotenvy::from_path_iter(&env_path) {
        Ok(iter) => {
            let vars: Vec<String> = iter
                .filter_map(|item| item.ok())
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            tracing::info!(project = project_id, count = vars.len(), "env var(s) loaded from .env");
            vars
        }
        Err(e) => {
            tracing::warn!(project = project_id, path = %env_path.display(), error = %e, "failed to parse .env");
            Vec::new()
        }
    }
}

/// Hash a file's raw content. Returns 0 if the file doesn't exist.
fn file_hash(path: &std::path::Path) -> u64 {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            hasher.finish()
        }
        Err(_) => 0,
    }
}

/// Hash the content portion of a .env.l8bin snapshot (strips the 5-line header).
/// Returns 0 if the file doesn't exist.
fn snapshot_content_hash(path: &std::path::Path) -> u64 {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    // Skip the header: 4 comment lines + 1 blank line
    let payload: String = content.lines().skip(5).collect::<Vec<_>>().join("\n");
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    hasher.finish()
}

/// Check if the project .env has changed since the last container creation.
/// Compares .env hash against .env.l8bin snapshot hash (header stripped).
pub fn env_has_changed(project_id: &str) -> bool {
    let env_path = projects_dir().join(project_id).join(".env");
    let snapshot_path = projects_dir().join(project_id).join(".env.l8bin");

    let env_hash = file_hash(&env_path);
    // .env.l8bin has a 5-line header (4 comments + blank line) — strip it before hashing
    let snapshot_hash = snapshot_content_hash(&snapshot_path);

    let changed = env_hash != snapshot_hash;
    tracing::info!(project = project_id, env_hash = env_hash, snapshot_hash = snapshot_hash, changed = changed, "env change check");
    changed
}

/// Write .env.l8bin snapshot — a copy of the current .env with a header.
/// Called after successfully creating a container with injected env vars.
pub fn write_env_snapshot(project_id: &str) {
    let env_path = projects_dir().join(project_id).join(".env");
    let snapshot_path = projects_dir().join(project_id).join(".env.l8bin");

    let content = match std::fs::read_to_string(&env_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let header = "# Auto-generated by LiteBin — do not edit manually.\n\
        # This records the env vars last injected into your container.\n\
        # Compare with .env to see pending changes.\n\
        # Docs: https://github.com/mtsandeep/l8bin/blob/main/docs/env-secrets.md\n";

    if let Err(e) = std::fs::write(&snapshot_path, format!("{}\n{}", header, content)) {
        tracing::warn!(project = project_id, error = %e, "failed to write .env.l8bin snapshot");
    } else {
        tracing::info!(project = project_id, "wrote .env.l8bin snapshot");
    }
}

// ── Project Metadata ─────────────────────────────────────────────────────────

/// Metadata needed to recreate a container without asking the orchestrator.
#[derive(Serialize, Deserialize, Clone)]
pub struct ProjectMetadata {
    pub image: String,
    pub internal_port: i64,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub volumes: Option<Vec<VolumeMount>>,
}

/// Path to the metadata file for a project.
pub fn metadata_path(project_id: &str) -> std::path::PathBuf {
    projects_dir().join(project_id).join("metadata.json")
}

/// Write project metadata to disk after successful container creation.
pub fn write_project_metadata(project_id: &str, image: &str, internal_port: i64, cmd: Option<&str>, memory_limit_mb: Option<i64>, cpu_limit: Option<f64>, volumes: Option<Vec<VolumeMount>>) {
    let meta = ProjectMetadata {
        image: image.to_string(),
        internal_port,
        cmd: cmd.map(|s| s.to_string()),
        memory_limit_mb,
        cpu_limit,
        volumes,
    };
    let path = metadata_path(project_id);
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&meta).unwrap_or_default()) {
        tracing::warn!(project = project_id, error = %e, "failed to write metadata.json");
    } else {
        tracing::info!(project = project_id, "wrote metadata.json");
    }
}

/// Read project metadata from disk. Returns None if file doesn't exist or is invalid.
pub fn read_project_metadata(project_id: &str) -> Option<ProjectMetadata> {
    let path = metadata_path(project_id);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /containers/run
/// Pull image and run container. Docker auto-assigns the host port.
/// Returns container_id and mapped_port.
pub async fn run_container(
    State(state): State<AgentState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    // Remove existing container for this project if present (handles redeploy)
    let _ = state.docker.remove_by_name(&req.project_id).await;

    // Pull image before running
    if let Err(e) = state.docker.pull_image(&req.image).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("failed to pull image: {e}"),
            }),
        )
            .into_response();
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
        status: "deploying".to_string(),
        cmd: req.cmd.clone(),
        memory_limit_mb: req.memory_limit_mb,
        cpu_limit: req.cpu_limit,
        custom_domain: None,
        volumes: req.volumes.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        last_active_at: None,
        service_count: None,
        service_summary: None,
        created_at: 0,
        updated_at: 0,
    };

    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
    match state.docker.run_service_container(&config).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            let _ = super::waker::rebuild_local_caddy(&state).await;
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
        status: "deploying".to_string(),
        cmd: req.cmd.clone(),
        memory_limit_mb: req.memory_limit_mb,
        cpu_limit: req.cpu_limit,
        custom_domain: None,
        volumes: req.volumes.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        last_active_at: None,
        service_count: None,
        service_summary: None,
        created_at: 0,
        updated_at: 0,
    };

    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
    match state.docker.run_service_container(&config).await {
        Ok((container_id, mapped_port)) => {
            // Rebuild agent Caddy config so the new container gets a route
            let _ = super::waker::rebuild_local_caddy(&state).await;
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
                status: "running".to_string(),
                cmd: req.cmd.clone(),
                memory_limit_mb: req.memory_limit_mb,
                cpu_limit: req.cpu_limit,
                custom_domain: None,
                volumes: None, // start uses existing container, volumes unchanged
                auto_stop_enabled: false,
                auto_stop_timeout_mins: 0,
                auto_start_enabled: false,
                last_active_at: None,
                service_count: None,
                service_summary: None,
                created_at: 0,
                updated_at: 0,
            };

            let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
            return match state.docker.run_service_container(&config).await {
                Ok((_container_id, mapped_port)) => {
                    let _ = super::waker::rebuild_local_caddy(&state).await;
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
    let _ = super::waker::rebuild_local_caddy(&state).await;

    // Inspect to return the mapped port
    match state.docker.inspect_mapped_port(&req.container_id).await {
        Ok(mapped_port) => (
            StatusCode::OK,
            Json(StartResponse { mapped_port }),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(container_id = %req.container_id, error = %e, "started container but failed to inspect port");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("container started but port inspection failed: {e}"),
                }),
            )
                .into_response()
        }
    }
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

/// GET /containers/:id/status
pub async fn container_status(
    State(state): State<AgentState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let inspect = match state.docker.inspect_container(&id).await {
        Ok(info) => info,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "container not found".to_string(),
                }),
            )
                .into_response();
        }
    };

    let container_state = inspect
        .state
        .as_ref()
        .and_then(|s| s.status.as_ref())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Extract mapped port from inspect
    let mapped_port = inspect
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .and_then(|ports| {
            ports.values().find_map(|bindings| {
                bindings.as_ref()?.first().and_then(|b| {
                    b.host_port
                        .as_ref()
                        .and_then(|p| p.parse::<u16>().ok())
                })
            })
        });

    // Get CPU/memory stats
    let stats = state.docker.container_stats(&id).await.unwrap_or(
        litebin_common::docker::ContainerStats {
            cpu_percent: 0.0,
            memory_usage: 0,
            memory_limit: 0,
        },
    );

    let status = ContainerStatus {
        state: container_state,
        mapped_port,
        cpu_percent: stats.cpu_percent,
        memory_usage: stats.memory_usage,
        memory_limit: stats.memory_limit,
    };

    (StatusCode::OK, Json(status)).into_response()
}

/// GET /containers/:id/logs?tail=100
pub async fn container_logs(
    State(state): State<AgentState>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let tail = query.tail.unwrap_or(100);

    match state.docker.container_logs(&id, tail).await {
        Ok(lines) => {
            let body = lines.join("");
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                body,
            )
                .into_response()
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

/// GET /containers/:id/disk-usage
pub async fn container_disk_usage(
    State(state): State<AgentState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.docker.disk_usage(&id).await {
        Ok(usage) => (StatusCode::OK, Json(usage)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct BatchStatsRequest {
    pub container_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct ContainerStatsResponse {
    pub container_id: String,
    pub state: String,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub disk_gb: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<f64>,
}

/// POST /containers/stats
/// Returns stats + disk for multiple containers in one call.
pub async fn batch_container_stats(
    State(state): State<AgentState>,
    Json(req): Json<BatchStatsRequest>,
) -> impl IntoResponse {
    let handles: Vec<_> = req
        .container_ids
        .into_iter()
        .map(|id| {
            let docker = state.docker.clone();
            tokio::spawn(async move {
                // Check running state
                let is_running = docker.is_container_running(&id).await.unwrap_or(false);
                if !is_running {
                    let disk = docker.disk_usage(&id).await.unwrap_or_else(|_| litebin_common::docker::DiskUsage {
                        size_rw: 0, size_root_fs: 0, cpu_limit: None,
                    });
                    return ContainerStatsResponse {
                        container_id: id,
                        state: "stopped".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: disk.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0),
                        cpu_limit: disk.cpu_limit,
                    };
                }

                // Fetch stats + disk in parallel
                let stats_fut = docker.container_stats(&id);
                let disk_fut = docker.disk_usage(&id);
                let (stats_res, disk_res) = tokio::join!(stats_fut, disk_fut);

                let stats = stats_res.unwrap_or(litebin_common::docker::ContainerStats {
                    cpu_percent: 0.0,
                    memory_usage: 0,
                    memory_limit: 0,
                });

                let disk = disk_res.unwrap_or_else(|_| litebin_common::docker::DiskUsage {
                    size_rw: 0, size_root_fs: 0, cpu_limit: None,
                });

                ContainerStatsResponse {
                    container_id: id,
                    state: "running".to_string(),
                    cpu_percent: stats.cpu_percent,
                    memory_usage: stats.memory_usage,
                    memory_limit: stats.memory_limit,
                    disk_gb: disk.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0),
                    cpu_limit: disk.cpu_limit,
                }
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.unwrap_or_else(|_| ContainerStatsResponse {
            container_id: "unknown".to_string(),
            state: "error".to_string(),
            cpu_percent: 0.0,
            memory_usage: 0,
            memory_limit: 0,
            disk_gb: 0.0,
            cpu_limit: None,
        }));
    }

    (StatusCode::OK, Json(results)).into_response()
}

// ── Multi-Service Batch Run ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BatchRunRequest {
    pub project_id: String,
    pub compose_yaml: String,
    /// Ordered list of service names to start (topologically sorted by orchestrator).
    pub service_order: Vec<String>,
    /// If Some, only recreate these services (partial redeploy). If None, deploy all.
    pub target_services: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct BatchRunResponse {
    pub services: Vec<ServiceRunResult>,
}

#[derive(Serialize)]
pub struct ServiceRunResult {
    pub service_name: String,
    pub container_id: Option<String>,
    pub mapped_port: Option<u16>,
    pub error: Option<String>,
}

/// POST /containers/batch-run
/// Deploy a multi-service project on the agent: store compose, pull images, start in order.
pub async fn batch_run(
    State(state): State<AgentState>,
    Json(req): Json<BatchRunRequest>,
) -> impl IntoResponse {
    tracing::info!(
        project = %req.project_id,
        services = ?req.service_order,
        "batch-run request received"
    );

    // Ensure project directory exists
    ensure_project_dir_and_env(&req.project_id);

    // Store compose.yaml
    let compose_path = projects_dir().join(&req.project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &req.compose_yaml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to store compose.yaml: {e}") }),
        ).into_response();
    }

    let extra_env = read_project_env(&req.project_id);

    // Build compose run plan (parse, topo sort, detect public, map configs)
    let plan = match litebin_common::compose_run::build_compose_run_plan(
        &req.compose_yaml, &req.project_id, &extra_env, None,
    ) {
        Ok(p) => p,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: format!("invalid compose: {e}") }),
        ).into_response(),
    };

    // Clean up existing containers from a previous deploy (by name prefix)
    // On partial redeploy, only remove targeted service containers
    let prefix = format!("litebin-{}.", req.project_id);
    let target_set: Option<std::collections::HashSet<String>> = req.target_services.as_ref()
        .map(|ts| ts.iter().cloned().collect());
    if let Ok(all_containers) = state.docker.list_containers_by_prefix(&prefix).await {
        for cid in &all_containers {
            if let Some(ref targets) = target_set {
                // Partial redeploy: check if this container belongs to a target service
                if let Ok(inspect) = state.docker.inspect_container(cid).await {
                    if let Some(ref name) = inspect.name {
                        let trimmed = name.trim_start_matches('/');
                        if let Some(svc_name) = trimmed.strip_prefix(&prefix) {
                            if targets.contains(svc_name) {
                                let _ = state.docker.stop_container(cid).await;
                                let _ = state.docker.remove_container(cid).await;
                            }
                        }
                    }
                }
            } else {
                // Full deploy: remove all
                let _ = state.docker.stop_container(cid).await;
                let _ = state.docker.remove_container(cid).await;
            }
        }
    }

    // Ensure per-project network
    if let Err(e) = state.docker.ensure_project_network(&req.project_id, None).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to create project network: {e}") }),
        ).into_response();
    }

    // Connect Caddy to the project network
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(&req.project_id, None);
    let _ = state.docker.connect_container_to_network(&caddy_container, &project_network).await;

    // Pull images in parallel (only for target services on partial redeploy)
    let images_to_pull: Vec<String> = if let Some(ref targets) = target_set {
        plan.configs.iter()
            .filter(|c| targets.contains(&c.service_name))
            .map(|c| c.image.clone())
            .collect()
    } else {
        plan.configs.iter().map(|c| c.image.clone()).collect()
    };
    let pull_handles: Vec<_> = images_to_pull.into_iter().map(|image| {
        let docker = state.docker.clone();
        tokio::spawn(async move {
            (image.clone(), docker.pull_image(&image).await.map_err(|e| e.to_string()))
        })
    }).collect();

    for handle in pull_handles {
        if let Ok((image, result)) = handle.await {
            if let Err(e) = result {
                tracing::error!(image = %image, error = %e, "batch-run: failed to pull image");
            }
        }
    }

    // Build owned lookup: service_name -> RunServiceConfig
    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    // Start services level by level — parallel within each level
    let mut results: Vec<ServiceRunResult> = Vec::new();
    for level in &plan.service_levels {
        let mut tasks: JoinSet<ServiceRunResult> = JoinSet::new();

        for svc_name in level {
            // Apply target filter for partial redeploy
            if let Some(ref targets) = target_set {
                if !targets.contains(svc_name) {
                    continue;
                }
            }

            let run_config = configs_map[svc_name].clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let pid = req.project_id.clone();

            tasks.spawn(async move {
                match docker.run_service_container(&run_config).await {
                    Ok((container_id, mapped_port)) => {
                        tracing::info!(
                            project = %pid,
                            service = %svc,
                            container = %container_id,
                            port = %mapped_port,
                            "batch-run: service started"
                        );
                        ServiceRunResult {
                            service_name: svc,
                            container_id: Some(container_id),
                            mapped_port: Some(mapped_port),
                            error: None,
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            project = %pid,
                            service = %svc,
                            error = %e,
                            "batch-run: failed to start service"
                        );
                        ServiceRunResult {
                            service_name: svc,
                            container_id: None,
                            mapped_port: None,
                            error: Some(e.to_string()),
                        }
                    }
                }
            });
        }

        // Collect results from this level
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(r) => results.push(r),
                Err(e) => {
                    tracing::error!(error = %e, "batch-run: service task panicked");
                }
            }
        }
    }

    // Rebuild local Caddy with all running containers
    let _ = super::waker::rebuild_local_caddy(&state).await;

    (StatusCode::OK, Json(BatchRunResponse { services: results })).into_response()
}
