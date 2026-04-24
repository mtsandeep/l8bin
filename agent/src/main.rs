mod activity;
mod config;
mod routes;
mod tls;

use std::collections::HashMap;

use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use dashmap::DashMap;
use litebin_common::caddy::CaddyClient;
use litebin_common::docker::DockerManager;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::info;

use config::AgentRegistration;

pub struct WakeGuard {
    pub notify: Notify,
    pub success: std::sync::atomic::AtomicBool,
    pub completed: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct AgentState {
    pub config: Arc<config::Config>,
    pub docker: Arc<DockerManager>,
    pub caddy: Option<Arc<CaddyClient>>,
    pub wake_locks: Arc<DashMap<String, Arc<WakeGuard>>>,
    pub registration: Arc<std::sync::RwLock<Option<AgentRegistration>>>,
    pub last_caddy_config: Arc<std::sync::RwLock<Option<serde_json::Value>>>,
    pub project_meta: Arc<std::sync::RwLock<HashMap<String, bool>>>,  // project_id → auto_start_enabled
    // Reverse proxy client for multi-service projects (always routed through agent waker)
    pub proxy_client: reqwest::Client,
    // Per-project throttle for multi-service health checks (5s cooldown)
    pub multi_svc_health_check: Arc<DashMap<String, std::time::Instant>>,
}

const REGISTRATION_FILE: &str = "data/agent-state.json";
const CADDY_CONFIG_FILE: &str = "data/caddy-config.json";
const PROJECT_META_FILE: &str = "data/project-meta.json";

fn load_registration_from_file() -> Result<Option<AgentRegistration>> {
    let data = std::fs::read_to_string(REGISTRATION_FILE)?;
    let reg: AgentRegistration = serde_json::from_str(&data)?;
    tracing::info!(node_id = %reg.node_id, "loaded persisted registration from file");
    Ok(Some(reg))
}

pub fn save_registration_to_file(reg: &AgentRegistration) -> Result<()> {
    if let Some(parent) = std::path::Path::new(REGISTRATION_FILE).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(reg)?;
    std::fs::write(REGISTRATION_FILE, data)?;
    tracing::info!(node_id = %reg.node_id, "persisted registration to file");
    Ok(())
}

pub(crate) fn load_caddy_config_from_file() -> Option<serde_json::Value> {
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

pub(crate) fn load_project_meta_from_file() -> Option<HashMap<String, bool>> {
    let data = std::fs::read_to_string(PROJECT_META_FILE).ok()?;
    let meta: HashMap<String, bool> = serde_json::from_str(&data).ok()?;
    tracing::info!("loaded persisted project meta from file");
    Some(meta)
}

pub(crate) fn save_project_meta_to_file(meta: &HashMap<String, bool>) {
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

#[tokio::main]
async fn main() -> Result<()> {
    // Install crypto provider before any TLS operations
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Load .env if present
    let _ = dotenvy::dotenv();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut cfg = config::Config::from_env()?;

    // Auto-detect public IP if not set
    if cfg.public_ip.is_empty() {
        match litebin_common::net::detect_public_ip().await {
            Some(ip) => {
                tracing::info!(public_ip = %ip, "auto-detected public IP");
                cfg.public_ip = ip;
            }
            None => {
                tracing::warn!("could not auto-detect public IP; set PUBLIC_IP env var manually if needed");
            }
        }
    }

    let cfg = Arc::new(cfg);

    // Load persisted registration (if agent was previously registered)
    let registration: Arc<std::sync::RwLock<Option<AgentRegistration>>> =
        Arc::new(std::sync::RwLock::new(
            load_registration_from_file().ok().flatten(),
        ));

    // Ensure projects directory exists
    std::fs::create_dir_all("projects")?;

    // Init Docker manager
    let docker_network = std::env::var("DOCKER_NETWORK").unwrap_or_else(|_| "litebin-network".to_string());
    let memory_limit: i64 = 256 * 1024 * 1024;
    let cpu_limit: f64 = 0.5;
    let docker = Arc::new(DockerManager::new(
        docker_network,
        memory_limit,
        cpu_limit,
    )?);

    // Connect agent to all existing project networks so it can proxy to containers
    let agent_id = std::env::var("AGENT_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-agent".into());
    docker.connect_to_project_networks(&agent_id).await;

    // Load persisted Caddy config (if orchestrator previously pushed one)
    let last_caddy_config: Arc<std::sync::RwLock<Option<serde_json::Value>>> =
        Arc::new(std::sync::RwLock::new(
            load_caddy_config_from_file(),
        ));

    // Load persisted project meta (project_id → auto_start_enabled)
    let project_meta: Arc<std::sync::RwLock<HashMap<String, bool>>> =
        Arc::new(std::sync::RwLock::new(
            load_project_meta_from_file().unwrap_or_default(),
        ));

    let state = AgentState {
        config: cfg.clone(),
        docker,
        caddy: if cfg.caddy_admin_url.is_empty() {
            None
        } else {
            Some(Arc::new(CaddyClient::new(&cfg.caddy_admin_url)))
        },
        wake_locks: Arc::new(DashMap::new()),
        registration: registration.clone(),
        last_caddy_config: last_caddy_config.clone(),
        project_meta: project_meta.clone(),
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
    };

    // Push persisted Caddy config on startup (so routes exist immediately)
    if let Some(caddy) = &state.caddy {
        let persisted = last_caddy_config.read().unwrap().clone();
        if let Some(config) = persisted {
            let url = format!("{}/load", caddy.admin_url());
            match caddy.post_json(&url, &config).await {
                Ok(resp) if resp.status().is_success() => {
                    info!("loaded persisted caddy config on startup");
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "failed to load persisted caddy config on startup");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load persisted caddy config on startup");
                }
            }
        } else {
            // No persisted config — push base config with TLS cert + catch-all 502
            // so agent Caddy has TLS ready for incoming master connections
            let base_config = routes::waker::build_base_caddy_config(
                &cfg.cert_pem,
                &cfg.key_pem,
            );
            let url = format!("{}/load", caddy.admin_url());
            match caddy.post_json(&url, &base_config).await {
                Ok(resp) if resp.status().is_success() => {
                    info!("loaded base caddy config with TLS cert on startup");
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "failed to load base caddy config on startup");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load base caddy config on startup");
                }
            }
        }
    }

    // Spawn activity reporter (reports active hosts to orchestrator via UDP → HTTP)
    tokio::spawn(activity::run_activity_reporter(state.clone()));

    // Spawn internal wake server (HTTP, no TLS, Docker network only).
    // Used by agent Caddy to trigger wake for sleeping containers in cloudflare_dns mode.
    // Port 8444 is not exposed on the host — only reachable from the Docker network.
    {
        let wake_state = state.clone();
        tokio::spawn(async move {
            let wake_addr = SocketAddr::from(([0, 0, 0, 0], 8444));
            let wake_app = Router::new()
                .route("/internal/caddy-ask", get(routes::waker::caddy_ask))
                .fallback(routes::waker::wake)
                .with_state(wake_state);
            info!("Starting internal wake server on {}", wake_addr);
            match tokio::net::TcpListener::bind(wake_addr).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, wake_app).await {
                        tracing::error!(error = %e, "internal wake server failed");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to bind internal wake server on port 8444");
                }
            }
        });
    }

    let app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/internal/register", post(routes::register::register))
        .route("/internal/project-meta", post(routes::project_meta::update_project_meta))
        .route("/containers/run", post(routes::containers::run_container))
        .route("/containers/recreate", post(routes::containers::recreate_container))
        .route("/containers/start", post(routes::containers::start_container))
        .route("/containers/stop", post(routes::containers::stop_container))
        .route("/containers/remove", post(routes::containers::remove_container))
        .route("/containers/{id}/status", get(routes::containers::container_status))
        .route("/containers/{id}/logs", get(routes::containers::container_logs))
        .route("/containers/{id}/disk-usage", get(routes::containers::container_disk_usage))
        .route("/containers/stats", post(routes::containers::batch_container_stats))
        .route("/containers/batch-run", post(routes::containers::batch_run))
        .route("/containers/cleanup", post(routes::containers::cleanup_project))
        .route("/images/load", post(routes::images::load_image))
        .route("/images/remove-unused", post(routes::images::remove_unused_image))
        .route("/images/prune", post(routes::images::prune_images))
        .route("/volumes/export", post(routes::volumes::export_volume))
        .route("/volumes/import", post(routes::volumes::import_volume))
        .route("/caddy/sync", post(routes::caddy::sync_caddy))
        .fallback(routes::waker::wake)
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.agent_port));

    // mTLS is required. Agent will not start without valid certificates.
    let cert_path = std::path::Path::new(&cfg.cert_path);
    let key_path = std::path::Path::new(&cfg.key_path);
    let ca_path = std::path::Path::new(&cfg.ca_cert_path);

    if !cert_path.exists() || !key_path.exists() || !ca_path.exists() {
        anyhow::bail!(
            "mTLS certificates not found. Required files:\n  cert: {}\n  key: {}\n  ca: {}\nRun the agent installer: curl -fsSL https://l8b.in | bash -s agent",
            cfg.cert_path, cfg.key_path, cfg.ca_cert_path
        );
    }

    info!("Starting agent with mTLS on https://{}", addr);
    let tls_config = tls::build_server_tls_config(
        &cfg.cert_path,
        &cfg.key_path,
        &cfg.ca_cert_path,
    )?;
    let rustls_config =
        axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));
    axum_server::bind_rustls(addr, rustls_config)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}
