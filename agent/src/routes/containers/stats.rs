use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use litebin_common::docker::DockerErrorKind;
use litebin_common::types::ContainerStatus;
use serde::{Deserialize, Serialize};

use crate::AgentState;

use super::types::{ErrorResponse, LogsQuery};

// ── Stats types ──────────────────────────────────────────────────────────────

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

// ── Container Inspection / Stats Handlers ────────────────────────────────────

/// GET /containers/:id/status
pub async fn container_status(State(state): State<AgentState>, Path(id): Path<String>) -> impl IntoResponse {
    let inspect = match state.docker.inspect_container(&id).await {
        Ok(info) => info,
        Err(e) => {
            let (status, message) = match DockerErrorKind::from_anyhow(&e) {
                DockerErrorKind::NotFound => (StatusCode::NOT_FOUND, "container not found".to_string()),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to inspect container: {e}")),
            };
            return (status, Json(ErrorResponse { error: message })).into_response();
        }
    };

    let container_state = inspect
        .state
        .as_ref()
        .and_then(|s| s.status.as_ref())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Extract mapped port from inspect
    let mapped_port = inspect.network_settings.as_ref().and_then(|ns| ns.ports.as_ref()).and_then(|ports| {
        ports.values().find_map(|bindings| {
            bindings.as_ref()?.first().and_then(|b| b.host_port.as_ref().and_then(|p| p.parse::<u16>().ok()))
        })
    });

    // Get CPU/memory stats
    let stats = state.docker.container_stats(&id).await.unwrap_or(litebin_common::docker::ContainerStats {
        cpu_percent: 0.0,
        memory_usage: 0,
        memory_limit: 0,
    });

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
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], body).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response(),
    }
}

/// GET /containers/:id/disk-usage
pub async fn container_disk_usage(State(state): State<AgentState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.docker.disk_usage(&id).await {
        Ok(usage) => (StatusCode::OK, Json(usage)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })).into_response(),
    }
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
                        size_rw: 0,
                        size_root_fs: 0,
                        cpu_limit: None,
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
                    size_rw: 0,
                    size_root_fs: 0,
                    cpu_limit: None,
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
