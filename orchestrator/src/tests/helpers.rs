use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_login::AuthManagerLayerBuilder;
use axum_test::{TestServer, TestServerConfig};
use dashmap::DashMap;
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::SqliteStore;

use crate::auth::backend::PasswordBackend;
use crate::config::Config;
use crate::AppState;
use litebin_common::docker::DockerManager;
use litebin_common::routing::{MasterProxyRouter, RoutingProvider};

/// Spin up an in-memory DB, run migrations, and return both the TestServer and the DB pool.
/// Use this when tests need to insert rows directly (e.g. to set up project fixtures).
pub async fn test_server_with_db() -> (TestServer, SqlitePool) {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO nodes (id, name, host, agent_port, status, fail_count, created_at, updated_at)
         VALUES ('local', 'Local', 'localhost', 0, 'online', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();

    let config = Arc::new(test_config());
    let docker = Arc::new(DockerManager::new_for_tests());
    let router: Arc<RwLock<Arc<dyn RoutingProvider>>> = Arc::new(RwLock::new(
        Arc::new(MasterProxyRouter::new(litebin_common::caddy::CaddyClient::new("http://localhost:2019"), String::new()))
    ));

    let state = AppState {
        config: config.clone(),
        db: db.clone(),
        docker,
        router,
        node_clients: Arc::new(DashMap::new()),
        disk_cache: Arc::new(DashMap::new()),
        deploy_locks: Arc::new(DashMap::new()),
        wake_locks: Arc::new(DashMap::new()),
        route_sync_tx: tokio::sync::mpsc::unbounded_channel().0,
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
    };

    let app = build_router(state);

    let config = TestServerConfig {
        save_cookies: true,
        ..TestServerConfig::new()
    };
    let server = TestServer::new_with_config(app, config).unwrap();
    (server, db)
}

/// Spin up an in-memory DB, run migrations, and return a TestServer with the full router.
pub async fn test_server() -> TestServer {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();

    // Register local node so select_node works
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO nodes (id, name, host, agent_port, status, fail_count, created_at, updated_at)
         VALUES ('local', 'Local', 'localhost', 0, 'online', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();

    let config = Arc::new(test_config());

    // Use the test constructor that doesn't connect to the Docker socket
    let docker = Arc::new(DockerManager::new_for_tests());
    let router: Arc<RwLock<Arc<dyn RoutingProvider>>> = Arc::new(RwLock::new(
        Arc::new(MasterProxyRouter::new(litebin_common::caddy::CaddyClient::new("http://localhost:2019"), String::new()))
    ));

    let state = AppState {
        config: config.clone(),
        db: db.clone(),
        docker,
        router,
        node_clients: Arc::new(DashMap::new()),
        disk_cache: Arc::new(DashMap::new()),
        deploy_locks: Arc::new(DashMap::new()),
        wake_locks: Arc::new(DashMap::new()),
        route_sync_tx: tokio::sync::mpsc::unbounded_channel().0,
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
    };

    let app = build_router(state);

    let config = TestServerConfig {
        save_cookies: true,
        ..TestServerConfig::new()
    };
    TestServer::new_with_config(app, config).unwrap()
}

/// Build the full router for tests.
/// Uses a custom 401-returning auth guard instead of the redirect-based login_required!.
pub fn build_router(state: AppState) -> Router {
    use axum::routing::{delete, get, patch, post, put};
    use tower_http::cors::CorsLayer;

    use crate::routes;

    let session_store = SqliteStore::new(state.db.clone());
    let session_layer = SessionManagerLayer::new(session_store).with_secure(false);
    let backend = PasswordBackend::new(state.db.clone());
    let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer).build();

    // Auth-public routes (no login required)
    let auth_public = Router::new()
        .route("/auth/login", post(routes::auth::login))
        .route("/auth/register", post(routes::auth::register));

    // Auth-protected routes — use axum_login::login_required! but map 307 → 401
    // by wrapping with a middleware that intercepts redirects to /auth/login.
    let auth_protected = Router::new()
        .route("/auth/logout", post(routes::auth::logout))
        .route("/auth/me", get(routes::auth::me))
        .route("/auth/change-password", post(routes::auth::change_password))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth_or_401,
        ));

    let api_routes = Router::new()
        .route("/projects", get(routes::projects::list_projects))
        .route("/projects/{id}", get(routes::projects::get_project))
        .route("/projects/{id}/settings", patch(routes::settings::update_project_settings))
        .route("/projects/{id}/stop", post(routes::manage::stop_project))
        .route("/projects/{id}/start", post(routes::manage::start_project))
        .route("/projects/{id}", delete(routes::manage::delete_project))
        .route("/projects/{id}/stats", get(routes::stats::project_stats))
        .route("/projects/{id}/logs", get(routes::stats::project_logs))
        .route("/nodes", get(routes::nodes::list_nodes))
        .route("/nodes", post(routes::nodes::create_node))
        .route("/nodes/{id}", delete(routes::nodes::delete_node))
        .route("/nodes/{id}/connect", post(routes::nodes::connect_node))
        .route("/settings/cleanup-dns", post(routes::global_settings::cleanup_dns))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth_or_401,
        ));

    // Deploy + image upload (session OR deploy token auth — no login_required layer)
    let deploy_routes = Router::new()
        .route("/deploy", post(routes::deploy::deploy_create))
        .route("/deploy", put(routes::deploy::deploy_update))
        .route("/images/upload", post(routes::images::upload_image));

    // Deploy token management (session auth)
    let token_routes = Router::new()
        .route("/deploy-tokens", post(routes::deploy_tokens::create_token))
        .route("/deploy-tokens", get(routes::deploy_tokens::list_tokens))
        .route("/deploy-tokens/{id}", delete(routes::deploy_tokens::revoke_token))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth_or_401,
        ));

    Router::new()
        .merge(auth_public)
        .merge(auth_protected)
        .merge(api_routes)
        .merge(deploy_routes)
        .merge(token_routes)
        .route("/health", get(routes::health::health_check))
        .route("/internal/wake-report", post(routes::wake_report::wake_report))
        .fallback(routes::waker::wake)
        .layer(auth_layer)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Middleware: if the user is not authenticated, return 401 instead of redirecting.
async fn require_auth_or_401(
    auth_session: axum_login::AuthSession<PasswordBackend>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

fn test_config() -> Config {
    Config {
        domain: "localhost".to_string(),
        caddy_admin_url: "http://localhost:2019".to_string(),
        database_url: "sqlite::memory:".to_string(),
        docker_network: "litebin-test".to_string(),
        host: "127.0.0.1".to_string(),
        port: 5080,
        default_auto_stop_mins: 15,
        janitor_interval_secs: 300,
        flush_interval_secs: 60,
        ca_cert_path: String::new(),
        client_cert_path: String::new(),
        client_key_path: String::new(),
        heartbeat_interval_secs: 30,
        routing_mode: "master_proxy".to_string(),
        cloudflare_api_token: String::new(),
        cloudflare_zone_id: String::new(),
        public_ip: String::new(),
        dashboard_subdomain: "l8bin".to_string(),
        poke_subdomain: "poke".to_string(),
    }
}
