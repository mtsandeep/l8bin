use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub host: String,
    pub public_ip: Option<String>,
    pub agent_port: i64,
    pub region: Option<String>,
    pub status: String, // "pending_setup" | "online" | "offline" | "decommissioned"
    pub total_memory: Option<i64>,
    pub total_cpu: Option<f64>,
    pub available_memory: Option<i64>,
    pub disk_free: Option<i64>,
    pub disk_total: Option<i64>,
    pub container_count: i64,
    pub last_seen_at: Option<i64>,
    pub fail_count: i64,
    #[serde(skip_serializing)]
    pub agent_secret: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Project {
    pub id: String,
    pub user_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub internal_port: Option<i64>,
    pub mapped_port: Option<i64>,
    pub container_id: Option<String>,
    pub node_id: Option<String>,
    pub status: String,
    pub last_active_at: Option<i64>,
    pub auto_stop_enabled: bool,
    pub auto_stop_timeout_mins: i64,
    pub auto_start_enabled: bool,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub custom_domain: Option<String>,
    pub volumes: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A volume mount for bind-mounting host directories into containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    /// Path inside the container, e.g. "/app/uploads"
    pub path: String,
    /// Directory name under projects/{id}/data/. Defaults to project_id if omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub is_admin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Docker image statistics for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStats {
    pub dangling_count: u64,
    pub dangling_size: u64,
    pub in_use_count: u64,
    pub in_use_size: u64,
    pub total_count: u64,
    pub total_size: u64,
}

impl Default for ImageStats {
    fn default() -> Self {
        Self {
            dangling_count: 0,
            dangling_size: 0,
            in_use_count: 0,
            in_use_size: 0,
            total_count: 0,
            total_size: 0,
        }
    }
}

/// Agent health response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub version: String,
    pub memory_available: u64,
    pub memory_total: u64,
    pub cpu_cores: u32,
    pub disk_free: u64,
    pub disk_total: u64,
    pub container_count: u32,
    pub image_stats: ImageStats,
    pub public_ip: Option<String>,
}

/// Agent container status response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerStatus {
    pub state: String,
    pub mapped_port: Option<u16>,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}
