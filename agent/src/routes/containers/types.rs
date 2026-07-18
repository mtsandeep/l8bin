use litebin_common::types::VolumeMount;
use serde::{Deserialize, Serialize};

// ── Request / Response types ──────────────────────────────────────────────────

fn default_false() -> bool {
    false
}

#[derive(Deserialize)]
pub struct RunRequest {
    pub image: String,
    pub internal_port: Option<i64>,
    pub project_id: String,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub volumes: Option<Vec<VolumeMount>>,
    #[serde(default = "default_false")]
    pub docker_observe: bool,
    /// When true, only create the project directory, `.env` placeholder, and metadata.
    /// No image pull or container start.
    #[serde(default = "default_false")]
    pub stage_only: bool,
}

#[derive(Serialize)]
pub struct RunResponse {
    pub container_id: String,
    pub mapped_port: Option<u16>,
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
    #[serde(default = "default_false")]
    pub host_network: bool,
    #[serde(default = "default_false")]
    pub is_background: bool,
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
pub struct CleanupRequest {
    pub project_id: String,
    pub volumes: Vec<String>,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub tail: Option<usize>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}
