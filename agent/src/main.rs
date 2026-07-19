mod activity;
mod tls;

use anyhow::Result;
use axum::{Router, routing::get};
use dashmap::DashMap;
use litebin_agent::{
    AgentState, Config, build_router, load_caddy_config_from_file, load_project_meta_from_file,
    load_registration_from_file, routes,
};
use litebin_common::caddy::CaddyClient;
use litebin_common::docker::DockerManager;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // Install crypto provider before any TLS operations
    rustls::crypto::ring::default_provider().install_default().expect("Failed to install rustls crypto provider");

    // Load .env if present
    let _ = dotenvy::dotenv();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut cfg = Config::from_env()?;

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
    let registration = Arc::new(std::sync::RwLock::new(load_registration_from_file().ok().flatten()));

    // Ensure projects directory exists
    std::fs::create_dir_all("projects")?;

    // Init Docker manager (defaults from env vars, set by orchestrator/install.sh)
    let docker_network = std::env::var("DOCKER_NETWORK").unwrap_or_else(|_| "litebin-network".to_string());
    let memory_limit: i64 =
        std::env::var("DEFAULT_MEMORY_MB").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(256) * 1024 * 1024;
    let cpu_limit: f64 = std::env::var("DEFAULT_CPU_LIMIT").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.5);
    let mut docker = DockerManager::new(docker_network, memory_limit, cpu_limit)?;
    docker.detect_host_projects_dir().await;
    let removed_unsafe = docker.cleanup_unsafe_docker_socket_containers().await?;
    if removed_unsafe > 0 {
        tracing::warn!(count = removed_unsafe, "removed unsafe legacy Docker socket containers");
    }
    let docker = Arc::new(docker);

    // Connect agent to all existing project networks so it can proxy to containers
    let agent_id = std::env::var("AGENT_CONTAINER_NAME").unwrap_or_else(|_| "litebin-agent".into());
    docker.connect_to_project_networks(&agent_id).await;

    // Load persisted Caddy config (if orchestrator previously pushed one)
    let last_caddy_config: Arc<std::sync::RwLock<Option<serde_json::Value>>> =
        Arc::new(std::sync::RwLock::new(load_caddy_config_from_file()));

    // Load persisted project meta (project_id → auto_start_enabled + allow_raw_ports)
    let project_meta = Arc::new(std::sync::RwLock::new(load_project_meta_from_file().unwrap_or_default()));

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
            let base_config = routes::waker::build_base_caddy_config(&cfg.cert_pem, &cfg.key_pem);
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

    // Create shutdown signal channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn activity reporter (reports active hosts to orchestrator via UDP → HTTP)
    tokio::spawn(activity::run_activity_reporter(state.clone(), shutdown_rx.clone()));

    // Spawn internal wake server (HTTP, no TLS, Docker network only).
    // Used by agent Caddy to trigger wake for sleeping containers in cloudflare_dns mode.
    // Port 8444 is not exposed on the host — only reachable from the Docker network.
    {
        let wake_state = state.clone();
        let mut wake_shutdown_rx = shutdown_rx.clone();
        tokio::spawn(async move {
            let wake_addr = SocketAddr::from(([0, 0, 0, 0], 8444));
            let wake_app = Router::new()
                .route("/internal/caddy-ask", get(routes::waker::caddy_ask))
                .fallback(routes::waker::wake)
                .with_state(wake_state);
            info!("Starting internal wake server on {}", wake_addr);
            match tokio::net::TcpListener::bind(wake_addr).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, wake_app)
                        .with_graceful_shutdown(async move {
                            let _ = wake_shutdown_rx.changed().await;
                        })
                        .await
                    {
                        tracing::error!(error = %e, "internal wake server failed");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to bind internal wake server on port 8444");
                }
            }
        });
    }

    let app = build_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.agent_port));

    // mTLS is required. Agent will not start without valid certificates.
    let cert_path = std::path::Path::new(&cfg.cert_path);
    let key_path = std::path::Path::new(&cfg.key_path);
    let ca_path = std::path::Path::new(&cfg.ca_cert_path);

    if !cert_path.exists() || !key_path.exists() || !ca_path.exists() {
        anyhow::bail!(
            "mTLS certificates not found. Required files:\n  cert: {}\n  key: {}\n  ca: {}\nRun the agent installer: curl -fsSL https://l8b.in | bash -s agent",
            cfg.cert_path,
            cfg.key_path,
            cfg.ca_cert_path
        );
    }

    let tls_config = tls::build_server_tls_config(&cfg.cert_path, &cfg.key_path, &cfg.ca_cert_path)?;
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));
    let handle = axum_server::Handle::new();

    // Spawn signal handler for graceful shutdown
    let shutdown_handle = handle.clone();
    let shutdown_signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received, draining connections...");
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
        let _ = shutdown_signal_tx.send(true);
    });

    tracing::info!(
        addr = %addr,
        node_id = ?registration.read().unwrap().as_ref().map(|r| &r.node_id),
        version = env!("CARGO_PKG_VERSION"),
        "agent startup complete — accepting connections"
    );

    axum_server::bind_rustls(addr, rustls_config).handle(handle).serve(app.into_make_service()).await?;

    // Wait briefly for background tasks to finish
    tracing::info!("shutdown complete");

    Ok(())
}

/// Wait for a shutdown signal (Ctrl+C on all platforms, SIGTERM on Unix).
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
