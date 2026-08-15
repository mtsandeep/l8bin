use serde::{Deserialize, Serialize};

use litebin_common::types::VolumeMount;

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeployResponse {
    pub status: String,
    pub project_id: String,
    /// Managed application URL. Background projects return `null`.
    pub url: Option<String>,
    pub message: String,
}

#[derive(Deserialize, Clone, utoipa::ToSchema)]
pub struct DeployRequest {
    pub project_id: String,
    pub image: String,
    pub port: Option<i64>,
    #[serde(default)]
    pub is_background: Option<bool>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_id: Option<String>, // optional override
    pub auto_stop_enabled: Option<bool>,
    pub auto_stop_timeout_mins: Option<i64>,
    pub auto_start_enabled: Option<bool>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub custom_domain: Option<String>,
    pub volumes: Option<Vec<VolumeMount>>,
    pub cleanup_volumes: Option<bool>,
    pub grant_capabilities: Option<Vec<String>>,
    /// When true on a first deploy, persist project metadata and create the runtime
    /// `.env` without starting containers. Ignored for redeploys of configured projects.
    #[serde(default)]
    pub stage_only: bool,
}
