use std::collections::HashMap;
use std::path::PathBuf;

use bollard::models::{ContainerCreateBody, HostConfig};
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
    pub service_count: Option<i64>,
    pub service_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A volume mount for a container (Docker named volume or host bind mount).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    /// Path inside the container, e.g. "/app/uploads"
    pub path: String,
    /// Volume source name. If starts with `/` or `./`, treated as a host bind mount path.
    /// Otherwise treated as a Docker named volume (scoped with project_id). Defaults to project_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Resolve a volume source to its final form.
/// - Named volumes: prefixed with `litebin_{project_id}_` (e.g. `pgdata` → `litebin_myproject_pgdata`)
/// - Relative bind mounts (`./`): resolved relative to `projects/{project_id}/` (e.g. `./data` → `projects/myproject/data`)
/// - Absolute bind mounts (`/`): passed through unchanged
pub fn scope_volume_source(name: &str, project_id: &str) -> String {
    if name.starts_with('/') {
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

// ── Multi-Service Types ─────────────────────────────────────────────────────

/// A service within a project (one row per service in `project_services`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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
    pub status: String,
    pub instance_id: Option<String>,
}

/// A volume definition for a service (one row per mount in `project_volumes`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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
    pub binds: Option<Vec<String>>,
    pub is_public: bool,
    /// Pre-built bollard config from compose-bollard (compose path).
    /// When provided, these are used as the base and LiteBin overrides are applied on top.
    pub bollard_create_body: Option<ContainerCreateBody>,
    pub bollard_host_config: Option<HostConfig>,
}

impl RunServiceConfig {
    /// Build a `RunServiceConfig` from a `Project` (single-service "web" path).
    /// Converts Project fields into the unified service config format.
    pub fn from_project(project: &Project, extra_env: Vec<String>) -> Self {
        // Build volume specs from project volumes (named volumes + bind mounts)
        let binds: Option<Vec<String>> = if let Some(ref vols_json) = project.volumes {
            let mounts: Vec<VolumeMount> = serde_json::from_str(vols_json).unwrap_or_default();
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
            binds,
            is_public: true,
            bollard_create_body: None,
            bollard_host_config: None,
        }
    }
}

// ── Centralized Naming Functions ────────────────────────────────────────────
// All container names, network names, and data dirs go through these functions.
// Future naming changes are localized here.

/// Build the Docker container name for a service.
/// - Single-service (service_name="web", instance_id=None): `litebin-{project_id}`
/// - Multi-service (instance_id=None): `litebin-{project_id}-{service_name}`
/// - With instance: `litebin-{project_id}-{service_name}-{instance_id}`
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

/// Build the per-project Docker network name.
/// - Primary: `litebin-{project_id}`
/// - With instance: `litebin-{project_id}-{instance_id}`
pub fn project_network_name(project_id: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("litebin-{}-{}", project_id, id),
        None => format!("litebin-{}", project_id),
    }
}

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
