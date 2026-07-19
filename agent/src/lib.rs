mod config;
pub mod routes;

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use dashmap::DashMap;
use litebin_common::caddy::CaddyClient;
use litebin_common::docker::DockerManager;
use tokio::sync::Notify;

pub use config::{AgentRegistration, Config};

const REGISTRATION_FILE: &str = "data/agent-state.json";
const CADDY_CONFIG_FILE: &str = "data/caddy-config.json";
const PROJECT_META_FILE: &str = "data/project-meta.json";

pub struct WakeGuard {
    pub notify: Notify,
    pub success: std::sync::atomic::AtomicBool,
    pub completed: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct AgentState {
    pub config: Arc<Config>,
    pub docker: Arc<DockerManager>,
    pub caddy: Option<Arc<CaddyClient>>,
    pub wake_locks: Arc<DashMap<String, Arc<WakeGuard>>>,
    pub registration: Arc<std::sync::RwLock<Option<AgentRegistration>>>,
    pub last_caddy_config: Arc<std::sync::RwLock<Option<serde_json::Value>>>,
    pub project_meta: Arc<std::sync::RwLock<HashMap<String, ProjectMetaEntry>>>,
    pub proxy_client: reqwest::Client,
    pub multi_svc_health_check: Arc<DashMap<String, std::time::Instant>>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct ProjectMetaEntry {
    pub auto_start_enabled: bool,
    #[serde(default)]
    pub is_background: bool,
    #[serde(default)]
    pub allow_raw_ports: bool,
    #[serde(default)]
    pub docker_observe: bool,
    #[serde(default)]
    pub host_network: bool,
    #[serde(default)]
    pub default_memory_limit_mb: Option<i64>,
    #[serde(default)]
    pub default_cpu_limit: Option<f64>,
}

pub fn load_registration_from_file() -> anyhow::Result<Option<AgentRegistration>> {
    let data = std::fs::read_to_string(REGISTRATION_FILE)?;
    let reg: AgentRegistration = serde_json::from_str(&data)?;
    tracing::info!(node_id = %reg.node_id, "loaded persisted registration from file");
    Ok(Some(reg))
}

pub fn save_registration_to_file(reg: &AgentRegistration) -> anyhow::Result<()> {
    if let Some(parent) = std::path::Path::new(REGISTRATION_FILE).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(reg)?;
    std::fs::write(REGISTRATION_FILE, data)?;
    tracing::info!(node_id = %reg.node_id, "persisted registration to file");
    Ok(())
}

pub fn load_caddy_config_from_file() -> Option<serde_json::Value> {
    let data = std::fs::read_to_string(CADDY_CONFIG_FILE).ok()?;
    let config: serde_json::Value = serde_json::from_str(&data).ok()?;
    tracing::info!("loaded persisted caddy config from file");
    Some(config)
}

pub(crate) fn save_caddy_config_to_file(config: &serde_json::Value) {
    if let Some(parent) = std::path::Path::new(CADDY_CONFIG_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(config) {
        Ok(data) => {
            if let Err(e) = std::fs::write(CADDY_CONFIG_FILE, data) {
                tracing::warn!(error = %e, "failed to persist caddy config to file");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize caddy config"),
    }
}

pub fn load_project_meta_from_file() -> Option<HashMap<String, ProjectMetaEntry>> {
    let data = std::fs::read_to_string(PROJECT_META_FILE).ok()?;
    let meta: HashMap<String, ProjectMetaEntry> = serde_json::from_str(&data).ok()?;
    tracing::info!("loaded persisted project meta from file");
    Some(meta)
}

pub(crate) fn save_project_meta_to_file(meta: &HashMap<String, ProjectMetaEntry>) {
    if let Some(parent) = std::path::Path::new(PROJECT_META_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(meta) {
        Ok(data) => {
            if let Err(e) = std::fs::write(PROJECT_META_FILE, data) {
                tracing::warn!(error = %e, "failed to persist project meta to file");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize project meta"),
    }
}

/// Build the agent HTTP application around an already initialized state.
///
/// The production binary and integration tests use the same route handlers;
/// transport security is applied by the caller.
pub fn build_router(state: AgentState) -> Router {
    Router::new()
        .route("/health", get(routes::health::health))
        .route("/internal/register", post(routes::register::register))
        .route("/internal/project-meta", post(routes::project_meta::update_project_meta))
        .route("/containers/run", post(routes::containers::run_container))
        .route("/containers/recreate",post(routes::containers::recreate_container))
        .route("/containers/start", post(routes::containers::start_container))
        .route("/containers/stop", post(routes::containers::stop_container))
        .route("/containers/stop-service", post(routes::containers::stop_service))
        .route("/containers/stop-project", post(routes::containers::stop_project))
        .route("/containers/remove", post(routes::containers::remove_container))
        .route("/containers/{id}/status", get(routes::containers::container_status))
        .route("/containers/{id}/logs", get(routes::containers::container_logs))
        .route( "/containers/{id}/disk-usage", get(routes::containers::container_disk_usage))
        .route("/containers/stats", post(routes::containers::batch_container_stats))
        .route("/containers/batch-run", post(routes::containers::batch_run))
        .route("/containers/cleanup", post(routes::containers::cleanup_project))
        .route("/containers/scan", get(routes::containers::scan_containers))
        .route("/containers/import", post(routes::containers::import_containers))
        .route("/containers/compose-file", get(routes::containers::get_compose_file))
        .route("/images/load", post(routes::images::load_image))
        .route("/images/inspect", get(routes::images::inspect_image))
        .route("/images/remove-unused", post(routes::images::remove_unused_image))
        .route("/images/prune", post(routes::images::prune_images))
        .route("/volumes/export", post(routes::volumes::export_volume))
        .route("/volumes/import", post(routes::volumes::import_volume))
        .route("/caddy/sync", post(routes::caddy::sync_caddy))
        .fallback(routes::waker::wake)
        .with_state(state)
}
