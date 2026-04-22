use axum::Json;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

pub async fn health_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<HealthResponse> {
    // Quick Docker ping to verify connectivity
    let status = match state.docker.ping().await {
        Ok(_) => "ok".to_string(),
        Err(_) => "degraded (docker unreachable)".to_string(),
    };

    Json(HealthResponse {
        status,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// --- System stats for LiteBin stack services ---

const STACK_CONTAINERS: &[&str] = &[
    "litebin-orchestrator",
    "litebin-dashboard",
    "litebin-caddy",
];

#[derive(Serialize)]
pub struct ServiceStats {
    pub name: String,
    pub memory_usage: u64,
    pub cpu_percent: f64,
    pub disk_usage: u64,
}

#[derive(Serialize)]
pub struct SystemStatsResponse {
    pub services: Vec<ServiceStats>,
}

async fn fetch_service_stats(
    docker: &litebin_common::docker::DockerManager,
    container_name: &str,
) -> Option<ServiceStats> {
    let (stats_res, disk_res) = tokio::join!(
        docker.container_stats(container_name),
        docker.disk_usage(container_name),
    );

    let stats = stats_res.ok()?;
    let disk = disk_res.unwrap_or(litebin_common::docker::DiskUsage {
        size_rw: 0,
        size_root_fs: 0,
        cpu_limit: None,
    });

    Some(ServiceStats {
        name: container_name
            .strip_prefix("litebin-")
            .unwrap_or(container_name)
            .to_string(),
        memory_usage: stats.memory_usage,
        cpu_percent: stats.cpu_percent,
        disk_usage: disk.size_root_fs,
    })
}

pub async fn system_stats(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<SystemStatsResponse> {
    let docker = &state.docker;
    let mut services = Vec::new();

    let futures: Vec<_> = STACK_CONTAINERS
        .iter()
        .map(|name| fetch_service_stats(docker, name))
        .collect();

    let results = futures_util::future::join_all(futures).await;
    for s in results.into_iter().flatten() {
        services.push(s);
    }

    Json(SystemStatsResponse { services })
}
