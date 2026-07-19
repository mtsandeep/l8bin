use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use bollard::models::{ContainerCreateBody, HostConfig};

/// Well-known compose file names checked in priority order.
pub const COMPOSE_FILE_NAMES: &[&str] = &["compose.yaml", "docker-compose.yml", "compose.yml", "docker-compose.yaml"];

/// Reserved service name for LiteBin's managed Docker observation proxy.
pub const DOCKER_PROXY_SERVICE: &str = "litebin-docker-proxy";

// ── Port constants ───────────────────────────────────────────────────────────

pub const CADDY_HTTP_PORT: &str = "80";
pub const CADDY_HTTPS_PORT: &str = "443";
pub const CADDY_ADMIN_PORT: &str = "2019";
pub const DEFAULT_ORCHESTRATOR_PORT: &str = "5080";
pub const DEFAULT_AGENT_PORT: &str = "8443";
/// Conventional agent host-side mapping (`-p 5083:8443`); not env-discoverable.
pub const DEFAULT_AGENT_HOST_PORT: &str = "5083";

/// Host ports reserved by LiteBin's own services — never bound by app containers,
/// even with `allow_raw_ports` enabled.
pub fn litebin_reserved_host_ports() -> Vec<String> {
    vec![
        CADDY_HTTP_PORT.into(),
        CADDY_HTTPS_PORT.into(),
        CADDY_ADMIN_PORT.into(),
        std::env::var("PORT").unwrap_or_else(|_| DEFAULT_ORCHESTRATOR_PORT.into()),
        std::env::var("AGENT_PORT").unwrap_or_else(|_| DEFAULT_AGENT_PORT.into()),
        DEFAULT_AGENT_HOST_PORT.into(),
    ]
}
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};

// ── Status Enums ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ProjectStatus {
    Pending,
    Stopped,
    Running,
    Deploying,
    Importing,
    Stopping,
    Error,
    Degraded,
    Unconfigured,
    /// One-shot job finished with exit code 0 (Compose `service_completed_successfully`).
    Completed,
}

impl ProjectStatus {
    /// Transient statuses are managed by their owning code paths and should
    /// not be overwritten by periodic Docker reconciliation.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProjectStatus::Pending
                | ProjectStatus::Deploying
                | ProjectStatus::Importing
                | ProjectStatus::Stopping
                | ProjectStatus::Error
                | ProjectStatus::Unconfigured
        )
    }

    /// Whether this service status counts as healthy for project aggregation.
    pub fn is_service_healthy(&self) -> bool {
        matches!(self, ProjectStatus::Running | ProjectStatus::Completed)
    }
}

impl fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectStatus::Pending => write!(f, "pending"),
            ProjectStatus::Stopped => write!(f, "stopped"),
            ProjectStatus::Running => write!(f, "running"),
            ProjectStatus::Deploying => write!(f, "deploying"),
            ProjectStatus::Importing => write!(f, "importing"),
            ProjectStatus::Stopping => write!(f, "stopping"),
            ProjectStatus::Error => write!(f, "error"),
            ProjectStatus::Degraded => write!(f, "degraded"),
            ProjectStatus::Unconfigured => write!(f, "unconfigured"),
            ProjectStatus::Completed => write!(f, "completed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum NodeStatus {
    Online,
    Offline,
    PendingSetup,
    Decommissioned,
}

impl fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeStatus::Online => write!(f, "online"),
            NodeStatus::Offline => write!(f, "offline"),
            NodeStatus::PendingSetup => write!(f, "pending_setup"),
            NodeStatus::Decommissioned => write!(f, "decommissioned"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum RoutingMode {
    MasterProxy,
    CloudflareDns,
}

impl fmt::Display for RoutingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoutingMode::MasterProxy => write!(f, "master_proxy"),
            RoutingMode::CloudflareDns => write!(f, "cloudflare_dns"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum DeployType {
    Image,
    Compose,
}

impl fmt::Display for DeployType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeployType::Image => write!(f, "image"),
            DeployType::Compose => write!(f, "compose"),
        }
    }
}

// ── Domain Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub host: String,
    pub architecture: Option<String>,
    pub version: Option<String>,
    pub public_ip: Option<String>,
    pub agent_port: i64,
    pub region: Option<String>,
    pub status: NodeStatus,
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct Project {
    pub id: String,
    pub user_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_background: bool,
    pub image: Option<String>,
    pub internal_port: Option<i64>,
    pub mapped_port: Option<i64>,
    pub container_id: Option<String>,
    pub node_id: Option<String>,
    pub status: ProjectStatus,
    pub last_active_at: Option<i64>,
    pub auto_stop_enabled: bool,
    pub auto_stop_timeout_mins: i64,
    pub auto_start_enabled: bool,
    pub allow_raw_ports: bool,
    pub allow_docker_access: bool,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub custom_domain: Option<String>,
    pub volumes: Option<String>,
    pub service_count: Option<i64>,
    pub service_summary: Option<String>,
    pub deploy_type: Option<DeployType>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A volume mount for a container (Docker named volume or host bind mount).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VolumeMount {
    /// Path inside the container, e.g. "/app/uploads"
    pub path: String,
    /// Volume source name. If starts with `/` or `./`, treated as a host bind mount path.
    /// Otherwise treated as a Docker named volume (scoped with project_id). Defaults to project_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Serialize volume mounts to JSON string. Returns None on failure instead of
/// silently producing an empty string that would destroy volume data.
pub fn serialize_volumes(volumes: &[VolumeMount]) -> Option<String> {
    serde_json::to_string(volumes).ok()
}

/// Returns true if `s` starts with a Windows drive-letter path (e.g. "D:/" or "C:\").
pub fn is_windows_drive_path(s: &str) -> bool {
    s.len() >= 2
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.as_bytes()[1] == b':'
        && s.as_bytes().get(2).map(|&b| b == b'/' || b == b'\\').unwrap_or(false)
}

/// Resolve a volume source to its final form.
/// - Named volumes: prefixed with `litebin_{project_id}_` (e.g. `pgdata` → `litebin_myproject_pgdata`)
/// - Relative bind mounts (`./`): resolved relative to `projects/{project_id}/` (e.g. `./data` → `projects/myproject/data`)
/// - Absolute bind mounts (`/` or Windows drive letter): passed through unchanged
pub fn scope_volume_source(name: &str, project_id: &str) -> String {
    if name.starts_with('/') || is_windows_drive_path(name) {
        name.to_string()
    } else if name.starts_with("./") {
        let relative = name.strip_prefix("./").unwrap();
        format!("projects/{}/{}", project_id, relative)
    } else {
        format!("litebin_{}_{}", project_id, name)
    }
}

/// Classification of a volume source for cleanup purposes.
#[derive(Debug, Clone, PartialEq)]
pub enum VolumeKind {
    /// Docker named volume (e.g. `litebin_myproject_pgdata`)
    DockerVolume,
    /// Relative bind mount resolved by LiteBin (e.g. `projects/myproject/data`)
    RelativeBindMount,
    /// Absolute bind mount (e.g. `/host/path`) — user-managed, skip on delete
    AbsoluteBindMount,
}

/// Classify a scoped volume source name to determine cleanup strategy.
pub fn classify_volume(scoped_name: &str) -> VolumeKind {
    if scoped_name.starts_with("projects/") {
        VolumeKind::RelativeBindMount
    } else if scoped_name.starts_with('/') {
        VolumeKind::AbsoluteBindMount
    } else {
        VolumeKind::DockerVolume
    }
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
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
        Self { dangling_count: 0, dangling_size: 0, in_use_count: 0, in_use_size: 0, total_count: 0, total_size: 0 }
    }
}

/// Agent health response
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HealthReport {
    pub version: String,
    /// Agent API protocol version used for compatibility checks.
    pub protocol_version: u32,
    pub architecture: String,
    pub docker_rootless: Option<bool>,
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
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContainerStatus {
    pub state: String,
    pub mapped_port: Option<u16>,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}

// ── Multi-Service Types ─────────────────────────────────────────────────────

/// A service within a project (one row per service in `project_services`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct ProjectService {
    pub project_id: String,
    pub service_name: String,
    pub image: String,
    pub port: Option<i64>,
    pub cmd: Option<String>,
    pub is_public: bool,
    pub depends_on: Option<String>, // JSON: ["db", "redis"]
    pub container_id: Option<String>,
    pub mapped_port: Option<i64>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub status: ProjectStatus,
    pub instance_id: Option<String>,
    pub is_oneshot: bool,
}

/// A volume definition for a service (one row per mount in `project_volumes`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct ProjectVolume {
    pub project_id: String,
    pub service_name: String,
    pub volume_name: Option<String>,
    pub container_path: String,
}

/// Network configuration for a container (which networks + DNS aliases).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
}

/// Unified config for creating any service container.
/// Used by `run_service_container` — replaces direct `Project` usage.
#[derive(Debug, Clone)]
pub struct RunServiceConfig {
    pub project_id: String,
    pub service_name: String,
    /// None = primary instance (default). Some("staging") = scoped instance.
    pub instance_id: Option<String>,
    pub image: String,
    /// Internal port to expose. None = internal-only service (no port binding).
    pub port: Option<u16>,
    pub cmd: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub env: Vec<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub shm_size: Option<u64>,
    pub tmpfs: Option<HashMap<String, String>>,
    pub read_only: Option<bool>,
    pub extra_hosts: Option<Vec<String>>,
    pub networks: Option<Vec<NetworkConfig>>,
    /// Run in the host network namespace. Host-network services may not use
    /// managed/custom networks or Docker port publishing.
    pub host_network: bool,
    pub binds: Option<Vec<String>>,
    pub is_public: bool,
    /// True when another service depends on this one with
    /// `condition: service_completed_successfully`.
    pub is_oneshot: bool,
    /// Pre-built bollard config from compose-bollard (compose path).
    /// When provided, these are used as the base and LiteBin overrides are applied on top.
    pub bollard_create_body: Option<ContainerCreateBody>,
    pub bollard_host_config: Option<HostConfig>,
    /// When true, all ports from compose are bound directly on the host
    /// (bypassing Caddy). Only meaningful for compose services.
    pub allow_raw_ports: bool,
    /// True only for services approved to use LiteBin's read-only Docker observation proxy.
    pub docker_observe: bool,
    /// Internal marker authorizing the managed sidecar to mount the daemon socket.
    pub is_managed_docker_proxy: bool,
}

impl RunServiceConfig {
    /// Build a `RunServiceConfig` from a `Project` (single-service "web" path).
    /// Converts Project fields into the unified service config format.
    pub fn from_project(project: &Project, extra_env: Vec<String>) -> Self {
        // Build volume specs from project volumes (named volumes + bind mounts)
        let binds: Option<Vec<String>> = if let Some(ref vols_json) = project.volumes {
            let mounts: Vec<VolumeMount> = match serde_json::from_str(vols_json) {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(project = %project.id, error = %e, "failed to parse volumes JSON");
                    Vec::new()
                }
            };
            let built: Vec<String> = mounts
                .into_iter()
                .filter_map(|v| {
                    let name = v.name.as_deref().unwrap_or(&project.id);
                    Some(format!("{}:{}", scope_volume_source(name, &project.id), v.path))
                })
                .collect();
            if built.is_empty() { None } else { Some(built) }
        } else {
            None
        };

        Self {
            project_id: project.id.clone(),
            service_name: "web".to_string(),
            instance_id: None,
            image: project.image.clone().unwrap_or_default(),
            port: project.internal_port.map(|p| p as u16),
            cmd: project.cmd.clone(),
            entrypoint: None,
            working_dir: None,
            user: None,
            env: extra_env,
            memory_limit_mb: project.memory_limit_mb,
            cpu_limit: project.cpu_limit,
            shm_size: None,
            tmpfs: None,
            read_only: None,
            extra_hosts: None,
            networks: None,
            host_network: false,
            binds,
            is_public: !project.is_background,
            is_oneshot: false,
            bollard_create_body: None,
            bollard_host_config: None,
            allow_raw_ports: false,
            docker_observe: false,
            is_managed_docker_proxy: false,
        }
    }
}

// ── Centralized Naming Functions ────────────────────────────────────────────
// All container names, network names, and data dirs go through these functions.
// Future naming changes are localized here.

/// Build the Docker container name for a service.
/// - Single-service (service_name="web", instance_id=None): `litebin-{project_id}`
/// - Multi-service (instance_id=None): `litebin-{project_id}.{service_name}`
/// - With instance: `litebin-{project_id}.{service_name}.{instance_id}`
pub fn container_name(project_id: &str, service_name: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("litebin-{}.{}.{}", project_id, service_name, id),
        None => {
            if service_name == "web" {
                format!("litebin-{}", project_id)
            } else {
                format!("litebin-{}.{}", project_id, service_name)
            }
        }
    }
}

/// Return the deterministic primary container name when the identity can be
/// represented unambiguously by LiteBin's naming convention.
pub fn primary_service_container_name(project_id: &str, service_name: &str) -> Option<String> {
    let valid_project_id = !project_id.is_empty()
        && project_id.len() <= 63
        && !project_id.starts_with('-')
        && !project_id.ends_with('-')
        && project_id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let valid_service_name =
        !service_name.is_empty() && service_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid_project_id || !valid_service_name {
        return None;
    }

    let name = container_name(project_id, service_name, None);
    if is_primary_service_container_name(&name, project_id, service_name) { Some(name) } else { None }
}

/// Check whether a Docker name is the primary container for this exact service identity.
pub fn is_primary_service_container_name(name: &str, project_id: &str, service_name: &str) -> bool {
    parse_container_name(name).is_some_and(|(parsed_project, parsed_service, instance_id)| {
        parsed_project == project_id && parsed_service == service_name && instance_id.is_none()
    })
}

/// Build the per-project Docker network name.
/// - Primary: `litebin-{project_id}`
/// - With instance: `litebin-{project_id}-{instance_id}`
pub fn project_network_name(project_id: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("litebin-{}-{}", project_id, id),
        None => format!("litebin-{}", project_id),
    }
}

pub fn docker_observe_network_name(project_id: &str, instance_id: Option<&str>) -> String {
    format!("{}-docker-observe", project_network_name(project_id, instance_id))
}

/// Managed image used for the endpoint-allowlisted Docker observation proxy.
pub const DOCKER_OBSERVE_PROXY_IMAGE: &str = "haproxy:3.0-alpine";
pub const DOCKER_OBSERVE_HAPROXY_CONFIG: &str = r#"global
    log stdout format raw local0

defaults
    log global
    mode http
    timeout connect 5s
    timeout client 1h
    timeout server 1h

frontend docker_observe
    bind *:2375
    acl read_method method GET HEAD
    acl observe_endpoint path_reg -i ^/(v[0-9.]+/)?(_ping|version|info|events|containers/json|containers/[^/]+/(json|stats|logs))$
    http-request deny deny_status 403 unless read_method
    http-request deny deny_status 403 unless observe_endpoint
    default_backend docker_socket

backend docker_socket
    server docker /var/run/docker.sock
"#;

/// Build the project data directory path.
/// - Primary: `projects/{project_id}/data/`
/// - With instance: `projects/{project_id}-{instance_id}/data/`
pub fn project_data_dir(project_id: &str, instance_id: Option<&str>) -> PathBuf {
    match instance_id {
        Some(id) => PathBuf::from("projects").join(format!("{}-{}", project_id, id)).join("data"),
        None => PathBuf::from("projects").join(project_id).join("data"),
    }
}

/// Parse a Docker container name back into (project_id, service_name, instance_id).
/// Handles: `litebin-{project_id}`, `litebin-{project_id}.{service}`, `litebin-{project_id}.{service}.{instance}`
pub fn parse_container_name(name: &str) -> Option<(String, String, Option<String>)> {
    let stripped = name.trim_start_matches('/');
    let rest = stripped.strip_prefix("litebin-")?;

    // Try 3-segment: project.service.instance (dot-delimited)
    if let Some((first, rest2)) = rest.split_once('.') {
        if let Some((second, third)) = rest2.split_once('.') {
            if !first.is_empty() && !second.is_empty() && !third.is_empty() {
                return Some((first.to_string(), second.to_string(), Some(third.to_string())));
            }
        }
    }

    // Try 2-segment: project.service
    if let Some((first, second)) = rest.split_once('.') {
        if !first.is_empty() && !second.is_empty() {
            return Some((first.to_string(), second.to_string(), None));
        }
    }

    // Single segment: just project_id (single-service with service "web")
    if !rest.is_empty() {
        return Some((rest.to_string(), "web".to_string(), None));
    }

    None
}

#[cfg(test)]
mod container_identity_tests {
    use super::{is_primary_service_container_name, primary_service_container_name};

    #[test]
    fn primary_service_identity_uses_canonical_names() {
        assert_eq!(primary_service_container_name("my-project", "web").as_deref(), Some("litebin-my-project"));
        assert_eq!(primary_service_container_name("my-project", "api").as_deref(), Some("litebin-my-project.api"));
    }

    #[test]
    fn primary_service_identity_rejects_ambiguous_identifiers() {
        assert_eq!(primary_service_container_name("my.project", "api"), None);
        assert_eq!(primary_service_container_name("my-project", "api.v2"), None);
        assert_eq!(primary_service_container_name("", "web"), None);
        assert_eq!(primary_service_container_name("my-project", ""), None);
        assert_eq!(primary_service_container_name("my-project", "../api"), None);
        assert_eq!(primary_service_container_name("MY-PROJECT", "api"), None);
    }

    #[test]
    fn primary_service_selection_is_exact_and_excludes_instances() {
        assert!(is_primary_service_container_name("/litebin-my-project.api", "my-project", "api"));
        assert!(!is_primary_service_container_name("/litebin-my-project.api.staging", "my-project", "api"));
        assert!(!is_primary_service_container_name("/litebin-my-project.api-v2", "my-project", "api"));
        assert!(!is_primary_service_container_name("/litebin-other.api", "my-project", "api"));
    }
}
