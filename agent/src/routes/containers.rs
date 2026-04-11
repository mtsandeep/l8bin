use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use litebin_common::types::ContainerStatus;
use serde::{Deserialize, Serialize};
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
}

#[derive(Serialize)]
pub struct RunResponse {
    pub container_id: String,
    pub mapped_port: u16,
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub container_id: String,
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

/// Read env vars from `projects/<project_id>/.env` if it exists.
fn read_project_env(project_id: &str) -> Vec<String> {
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
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        last_active_at: None,
        created_at: 0,
        updated_at: 0,
    };

    match state
        .docker
        .run_container(&project, extra_env, None)
        .await
    {
        Ok((container_id, mapped_port)) => {
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
        auto_stop_enabled: false,
        auto_stop_timeout_mins: 0,
        auto_start_enabled: false,
        last_active_at: None,
        created_at: 0,
        updated_at: 0,
    };

    match state
        .docker
        .run_container(&project, extra_env, None)
        .await
    {
        Ok((container_id, mapped_port)) => {
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
pub async fn start_container(
    State(state): State<AgentState>,
    Json(req): Json<StartRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.docker.start_existing_container(&req.container_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response();
    }

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
                    let disk_gb = docker.disk_usage(&id).await
                        .map(|d| d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0))
                        .unwrap_or(0.0);
                    return ContainerStatsResponse {
                        container_id: id,
                        state: "stopped".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb,
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

                let disk_gb = match disk_res {
                    Ok(d) => d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0),
                    Err(_) => 0.0,
                };

                ContainerStatsResponse {
                    container_id: id,
                    state: "running".to_string(),
                    cpu_percent: stats.cpu_percent,
                    memory_usage: stats.memory_usage,
                    memory_limit: stats.memory_limit,
                    disk_gb,
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
        }));
    }

    (StatusCode::OK, Json(results)).into_response()
}
