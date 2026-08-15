use serde::{Deserialize, Serialize};

pub(super) fn default_false() -> bool {
    false
}

pub(super) fn host_network_authorized(granted: bool, is_background: bool) -> bool {
    granted && is_background
}

#[derive(Deserialize)]
pub struct BatchRunRequest {
    pub project_id: String,
    pub compose_yaml: String,
    /// Ordered list of service names to start (topologically sorted by orchestrator).
    pub service_order: Vec<String>,
    /// If Some, only recreate these services (partial redeploy). If None, deploy all.
    pub target_services: Option<Vec<String>>,
    pub allow_raw_ports: Option<bool>,
    pub docker_observe: Option<bool>,
    pub host_network: Option<bool>,
    #[serde(default = "default_false")]
    pub is_background: bool,
    /// Whether to force-pull images (true) or skip if already present locally (false).
    #[serde(default = "default_false")]
    pub force_pull: bool,
    /// When true, only persist compose.yaml and create the runtime `.env` placeholder.
    /// No networks, pulls, or containers are started.
    #[serde(default = "default_false")]
    pub stage_only: bool,
    /// Per-service resource overrides from dashboard (service_name → {memory_limit_mb, cpu_limit}).
    /// Applied on top of compose-embedded limits; None values mean "use global default".
    pub service_resources: Option<std::collections::HashMap<String, ServiceResources>>,
    /// Global default memory limit (MB) from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a memory limit.
    pub default_memory_limit_mb: Option<i64>,
    /// Global default CPU limit from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a CPU limit.
    pub default_cpu_limit: Option<f64>,
}

/// Per-service resource overrides sent by the orchestrator.
#[derive(Deserialize)]
pub struct ServiceResources {
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
}

#[derive(Serialize)]
pub struct BatchRunResponse {
    pub services: Vec<ServiceRunResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ServiceRunResult {
    pub service_name: String,
    pub container_id: Option<String>,
    pub mapped_port: Option<u16>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct BatchRunErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affected_services: Vec<String>,
}
