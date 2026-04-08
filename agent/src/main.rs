mod config;
mod routes;
mod tls;

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
}

const REGISTRATION_FILE: &str = "data/agent-state.json";

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

#[tokio::main]
async fn main() -> Result<()> {
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
    let docker_network = std::env::var("DOCKER_NETWORK").unwrap_or_else(|_| "litebin-apps".to_string());
    let memory_limit: i64 = 256 * 1024 * 1024;
    let cpu_limit: f64 = 0.5;
    let docker = Arc::new(DockerManager::new(
        docker_network,
        memory_limit,
        cpu_limit,
    )?);

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
    };

    let app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/internal/register", post(routes::register::register))
        .route("/containers/run", post(routes::containers::run_container))
        .route("/containers/recreate", post(routes::containers::recreate_container))
        .route("/containers/start", post(routes::containers::start_container))
        .route("/containers/stop", post(routes::containers::stop_container))
        .route("/containers/remove", post(routes::containers::remove_container))
        .route("/containers/{id}/status", get(routes::containers::container_status))
        .route("/containers/{id}/logs", get(routes::containers::container_logs))
        .route("/containers/{id}/disk-usage", get(routes::containers::container_disk_usage))
        .route("/containers/stats", post(routes::containers::batch_container_stats))
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
            "mTLS certificates not found. Required files:\n  cert: {}\n  key: {}\n  ca: {}\nRun the agent installer: curl -sSL https://l8b.in | sh -s agent",
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
