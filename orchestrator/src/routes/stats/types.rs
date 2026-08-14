use serde::{Deserialize, Serialize};

// ServiceVolumeInfo / ServiceInfo are the orchestrator ↔ dashboard/CLI wire
// contract and live in litebin-common::types; re-exported here so the
// `routes::stats::` paths used by openapi.rs and projects stay stable.
pub use litebin_common::types::{ServiceInfo, ServiceVolumeInfo};

#[derive(Serialize, utoipa::ToSchema)]
pub struct StatsResponse {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceInfo>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BatchStatsResponse {
    pub stats: Vec<StatsResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DiskUsageResponse {
    pub project_id: String,
    pub size_gb: f64,
}

#[derive(Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct LogsQuery {
    pub tail: Option<usize>,
    pub service: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LogsResponse {
    pub project_id: String,
    pub service_name: Option<String>,
    pub lines: Vec<String>,
}

/// Per-container live stats collected from Docker.
/// (cpu_percent, memory_usage_bytes, memory_limit_bytes, disk_gb, cpu_limit)
pub(super) type LiveStats = (f64, u64, u64, f64, Option<f64>);
