mod auth;
mod cloudflare_router;
mod config;
mod db;
mod nodes;
mod routing_helpers;
mod routes;
mod sleep;
mod validation;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::{
    routing::{delete, get, patch, post},
    Router,
};
use axum_login::login_required;
use dashmap::DashMap;
use sqlx::SqlitePool;
use tokio::sync::{Notify, RwLock, Semaphore};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use config::Config;
use litebin_common::caddy::CaddyClient;
use litebin_common::cloudflare::CloudflareClient;
use litebin_common::docker::DockerManager;
use litebin_common::routing::{MasterProxyRouter, RoutingProvider};
use cloudflare_router::CloudflareDnsRouter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub docker: Arc<DockerManager>,
    pub router: Arc<RwLock<Arc<dyn RoutingProvider>>>,
    // Phase 6 additions:
    pub node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
    pub deploy_locks: Arc<DashMap<String, Arc<Semaphore>>>,
    pub wake_locks: Arc<DashMap<String, Arc<WakeGuard>>>,
    // Debounced route sync channel — send a signal to trigger a batched route sync
    pub route_sync_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

pub struct WakeGuard {
    pub notify: Notify,
    pub success: std::sync::atomic::AtomicBool,
    pub completed: std::sync::atomic::AtomicBool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present
    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "litebin_orchestrator=info,tower_http=info".into()),
        )
        .init();

    // Load config
    let mut config = Config::from_env()?;
    tracing::info!(port = %config.port, domain = %config.domain, "loading config");

    // Auto-detect public IP if not set
    if config.public_ip.is_empty() {
        match litebin_common::net::detect_public_ip().await {
            Some(ip) => {
                tracing::info!(public_ip = %ip, "auto-detected public IP");
                config.public_ip = ip;
            }
            None => {
                tracing::warn!("could not auto-detect public IP; set PUBLIC_IP env var manually if needed");
            }
        }
    }

    // Init database
    let db = db::init_pool(&config.database_url).await?;

    // Ensure projects directory exists
    std::fs::create_dir_all("projects")?;

    // Seed global settings if not already set
    let default_mem_mb: i64 = 256;
    let default_cpu_limit: f64 = 0.5;
    sqlx::query(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('default_memory_limit_mb', ?), ('default_cpu_limit', ?), ('routing_mode', ?), ('cloudflare_api_token', ?), ('cloudflare_zone_id', ?), ('dashboard_subdomain', ?), ('poke_subdomain', ?)"
    )
    .bind(default_mem_mb.to_string())
    .bind(default_cpu_limit.to_string())
    .bind(&config.routing_mode)
    .bind(&config.cloudflare_api_token)
    .bind(&config.cloudflare_zone_id)
    .bind(&config.dashboard_subdomain)
    .bind(&config.poke_subdomain)
    .execute(&db)
    .await?;
    tracing::info!(memory_mb = default_mem_mb, cpu = default_cpu_limit, "global settings seeded");

    // Auto-register local node
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO nodes (id, name, host, public_ip, agent_port, status, fail_count, created_at, updated_at)
         VALUES ('local', 'Local', 'localhost', ?, 0, 'online', 0, ?, ?)",
    )
    .bind(&config.public_ip)
    .bind(now)
    .bind(now)
    .execute(&db)
    .await?;
    tracing::info!(public_ip = %config.public_ip, "local node registered");

    // Init Docker manager
    let docker = DockerManager::new(
        config.docker_network.clone(),
        default_mem_mb * 1024 * 1024,
        default_cpu_limit,
    )?;

    // Initialize node client pool and load existing online nodes
    let node_clients: Arc<DashMap<String, Arc<reqwest::Client>>> = Arc::new(DashMap::new());

    let online_nodes = sqlx::query_as::<_, db::models::Node>(
        "SELECT * FROM nodes WHERE status = 'online' AND id != 'local'",
    )
    .fetch_all(&db)
    .await?;

    for node in online_nodes {
        match nodes::client::build_node_client(
            &config.ca_cert_path,
            &config.client_cert_path,
            &config.client_key_path,
        ) {
            Ok(client) => {
                node_clients.insert(node.id.clone(), Arc::new(client));
                tracing::info!(node_id = %node.id, "loaded node into client pool");
            }
            Err(e) => {
                tracing::warn!(node_id = %node.id, error = %e, "failed to build client for node");
            }
        }
    }

    let deploy_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());
    let wake_locks: Arc<DashMap<String, Arc<WakeGuard>>> = Arc::new(DashMap::new());
    // Verify Docker connectivity
    docker.ping().await?;
    tracing::info!("docker connection verified");

    // Seed local node with real system memory, cpu, and disk
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    let local_memory = sys.total_memory() as i64;
    let local_available = sys.available_memory() as i64;
    let local_cpu = sys.cpus().len() as f64;
    let (local_disk_free, local_disk_total) = litebin_common::sys::disk_space();
    let local_disk_free = local_disk_free as i64;
    let local_disk_total = local_disk_total as i64;
    let now_mem = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE nodes SET total_memory = ?, total_cpu = ?, available_memory = ?, disk_free = ?, disk_total = ?, public_ip = ?, last_seen_at = ?, updated_at = ? WHERE id = 'local'",
    )
    .bind(local_memory)
    .bind(local_cpu)
    .bind(local_available)
    .bind(local_disk_free)
    .bind(local_disk_total)
    .bind(&config.public_ip)
    .bind(now_mem)
    .bind(now_mem)
    .execute(&db)
    .await?;
    tracing::info!(memory_bytes = local_memory, available_bytes = local_available, disk_free_bytes = local_disk_free, disk_total_bytes = local_disk_total, "local node stats seeded");

    // Ensure the app network exists
    docker.ensure_network().await?;

    // Init routing provider
    let router = Arc::new(RwLock::new(build_routing_provider(
        &config.routing_mode,
        &config.cloudflare_api_token,
        &config.cloudflare_zone_id,
        &config.caddy_admin_url,
        node_clients.clone(),
        db.clone(),
        Arc::new(config.clone()),
    )));

    // Init debounced route sync channel
    let (route_sync_tx, route_sync_rx) = tokio::sync::mpsc::unbounded_channel();

    let state = AppState {
        config: Arc::new(config.clone()),
        db: db.clone(),
        docker: Arc::new(docker),
        router: router.clone(),
        node_clients,
        deploy_locks,
        wake_locks,
        route_sync_tx,
    };

    // Sync routes for any previously running projects (retry up to 5 times)
    let orchestrator_upstream = format!("litebin-orchestrator:{}", config.port);
    for attempt in 1..=5 {
        let routes = routing_helpers::resolve_all_routes(&db, &config.domain, &orchestrator_upstream).await.unwrap_or_default();
        let r = router.read().await.clone();
        match r
            .sync_routes(&routes, &config.domain, &orchestrator_upstream, &config.dashboard_subdomain, &config.poke_subdomain)
            .await
        {
            Ok(_) => break,
            Err(e) => {
                if attempt == 5 {
                    tracing::warn!(error = %e, "failed to sync routes after 5 attempts");
                } else {
                    tracing::info!(attempt, "caddy not ready, retrying in 2s...");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    // Spawn janitor background task
    tokio::spawn(sleep::janitor::run_janitor(state.clone()));

    // Spawn heartbeat background task
    tokio::spawn(nodes::heartbeat::run_heartbeat(state.clone()));

    // Spawn debounced route sync background task
    tokio::spawn(routing_helpers::run_route_sync(
        route_sync_rx,
        state.db.clone(),
        state.router.clone(),
        state.config.clone(),
    ));

    // Run startup reconciliation pass
    nodes::reconciliation::run_reconciliation(state.clone(), None).await;

    // Routes - Auth public (no login required)
    let auth_public = Router::new()
        .route("/auth/login", post(routes::auth::login))
        .route("/auth/register", post(routes::auth::register))
        .route("/auth/setup", get(routes::auth::setup_check));

    // Routes - Auth protected (login required)
    let auth_protected = Router::new()
        .route("/auth/logout", post(routes::auth::logout))
        .route("/auth/me", get(routes::auth::me))
        .route("/auth/change-password", post(routes::auth::change_password))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Protected API (session auth only)
    let api_routes = Router::new()
        .route("/projects", post(routes::projects::create_project))
        .route("/projects", get(routes::projects::list_projects))
        .route("/projects/stats", get(routes::stats::all_project_stats))
        .route("/projects/{id}", get(routes::projects::get_project))
        .route("/projects/{id}/settings", patch(routes::settings::update_project_settings))
        .route("/projects/{id}/stop", post(routes::manage::stop_project))
        .route("/projects/{id}/start", post(routes::manage::start_project))
        .route("/projects/{id}", delete(routes::manage::delete_project))
        .route("/projects/{id}/stats", get(routes::stats::project_stats))
        .route("/projects/{id}/disk-usage", get(routes::stats::project_disk_usage))
        .route("/projects/{id}/logs", get(routes::stats::project_logs))
        .route("/projects/{id}/recreate", post(routes::manage::recreate_project))
        .route("/nodes", get(routes::nodes::list_nodes))
        .route("/nodes", post(routes::nodes::create_node))
        .route("/nodes/{id}", delete(routes::nodes::delete_node))
        .route("/nodes/{id}/connect", post(routes::nodes::connect_node))
        .route("/nodes/image-stats", get(routes::nodes::node_image_stats))
        .route("/nodes/{id}/images/prune", post(routes::nodes::prune_node_images))
        .route("/settings", get(routes::global_settings::get_settings))
        .route("/settings", patch(routes::global_settings::update_settings))
        .route("/settings/cleanup-dns", post(routes::global_settings::cleanup_dns))
        .route("/system/stats", get(routes::health::system_stats))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    // Routes - Deploy + image upload (session OR deploy token auth)
    let deploy_routes = Router::new()
        .route("/deploy", post(routes::deploy::deploy))
        .route("/images/upload", post(routes::images::upload_image));

    // Routes - Deploy token management (session auth)
    let token_routes = Router::new()
        .route("/deploy-tokens", post(routes::deploy_tokens::create_token))
        .route("/deploy-tokens", get(routes::deploy_tokens::list_tokens))
        .route("/deploy-tokens/{id}", delete(routes::deploy_tokens::revoke_token))
        .route_layer(login_required!(auth::backend::PasswordBackend, login_url = "/auth/login"));

    let app = Router::new()
        .merge(auth_public)
        .merge(auth_protected)
        .merge(api_routes)
        .merge(deploy_routes)
        .merge(token_routes)
        .route("/health", get(routes::health::health_check))
        .route("/caddy/ask", get(routes::caddy::ask))
        .route("/internal/wake-report", post(routes::wake_report::wake_report))
        .fallback(routes::waker::wake)
        .layer(auth::auth_layer(state.clone()))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!(addr = %addr, "starting server");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Construct the appropriate routing provider based on the given mode.
/// Used both at startup and during hot-swap when settings change.
pub(crate) fn build_routing_provider(
    routing_mode: &str,
    cf_token: &str,
    cf_zone: &str,
    caddy_admin_url: &str,
    node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
    db: SqlitePool,
    config: Arc<Config>,
) -> Arc<dyn RoutingProvider> {
    let caddy_client = CaddyClient::new(caddy_admin_url);
    match routing_mode {
        "cloudflare_dns" => {
            tracing::info!(zone_id = %cf_zone, "using cloudflare_dns routing mode");
            let cloudflare = CloudflareClient::new(cf_token, cf_zone);
            Arc::new(CloudflareDnsRouter::new(
                cloudflare,
                caddy_client,
                node_clients,
                db,
                config,
            ))
        }
        _ => Arc::new(MasterProxyRouter::new(caddy_client)),
    }
}
