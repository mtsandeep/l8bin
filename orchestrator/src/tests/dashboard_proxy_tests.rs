//! Dashboard fallback proxy — the orchestrator is the sole router for the dashboard host.
//! Unmatched paths on the dashboard host / bare domain are streamed to the dashboard
//! SPA upstream; API routes keep hitting their real handlers.

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;

use super::helpers::{test_config, test_server_with_db_config};

const HOST: axum::http::HeaderName = axum::http::header::HOST;

/// A stub dashboard upstream bound to a random loopback port.
struct StubDashboard {
    addr: String,
    /// Unblocks the second chunk of the /stream handler.
    release: tokio::sync::watch::Sender<bool>,
}

async fn spawn_stub_dashboard() -> StubDashboard {
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);

    async fn echo(req: axum::http::Request<axum::body::Body>) -> impl IntoResponse {
        let method = req.method().clone();
        let pq = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
        ([(axum::http::header::HeaderName::from_static("x-stub"), "yes")], format!("{method} {pq}"))
    }

    async fn stream(
        axum::extract::State(mut release): axum::extract::State<tokio::sync::watch::Receiver<bool>>,
    ) -> impl IntoResponse {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(axum::body::Bytes::from("chunk1"))).await;
            while !*release.borrow_and_update() {
                if release.changed().await.is_err() {
                    break;
                }
            }
            let _ = tx.send(Ok(axum::body::Bytes::from("chunk2"))).await;
        });
        axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
    }

    let app = Router::new().route("/stream", get(stream).with_state(release_rx)).fallback(echo);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    StubDashboard { addr: format!("{}:{}", addr.ip(), addr.port()), release: release_tx }
}

fn config_with_upstream(upstream: &str) -> crate::config::Config {
    let mut config = test_config();
    config.dashboard_upstream = upstream.to_string();
    config
}

#[tokio::test]
async fn dashboard_host_root_proxies_to_upstream() {
    let stub = spawn_stub_dashboard().await;
    let (server, _db) = test_server_with_db_config(config_with_upstream(&stub.addr)).await;

    let resp = server.get("/").add_header(HOST, "l8bin.localhost").await;

    resp.assert_status(StatusCode::OK);
    assert!(resp.headers().contains_key("x-stub"), "response must come from the stub, not the orchestrator");
    assert_eq!(resp.text(), "GET /");
}

#[tokio::test]
async fn dashboard_host_preserves_path_and_query() {
    let stub = spawn_stub_dashboard().await;
    let (server, _db) = test_server_with_db_config(config_with_upstream(&stub.addr)).await;

    let resp = server.get("/assets/app.js").add_query_param("v", "2").add_header(HOST, "l8bin.localhost").await;

    resp.assert_status(StatusCode::OK);
    assert_eq!(resp.text(), "GET /assets/app.js?v=2");
}

#[tokio::test]
async fn api_paths_still_hit_orchestrator_routes() {
    let stub = spawn_stub_dashboard().await;
    let (server, _db) = test_server_with_db_config(config_with_upstream(&stub.addr)).await;

    let resp = server.get("/health").add_header(HOST, "l8bin.localhost").await;

    resp.assert_status(StatusCode::OK);
    assert!(!resp.headers().contains_key("x-stub"), "API routes must not be swallowed by the proxy");
}

#[tokio::test]
async fn bare_and_loopback_hosts_proxy_to_dashboard() {
    let stub = spawn_stub_dashboard().await;
    let (server, _db) = test_server_with_db_config(config_with_upstream(&stub.addr)).await;

    for host in ["localhost", "127.0.0.1"] {
        let resp = server.get("/").add_header(HOST, host).await;
        resp.assert_status(StatusCode::OK);
        assert!(resp.headers().contains_key("x-stub"), "host {host} must reach the stub");
    }
}

/// A stopped project on an app subdomain must keep hitting the waker, not the dashboard proxy.
#[tokio::test]
async fn app_subdomain_still_wakes() {
    let stub = spawn_stub_dashboard().await;
    let (server, db) = test_server_with_db_config(config_with_upstream(&stub.addr)).await;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, is_admin, created_at, updated_at)
         VALUES ('test-user', 'testuser', 'hash', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, auto_start_enabled, last_active_at, created_at, updated_at)
           VALUES ('my-proj', 'test-user', 'test-image:latest', 8080, 'stopped', 0, ?, ?, ?)"#,
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();

    let resp = server.get("/").add_header(HOST, "my-proj.localhost").await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn unreachable_dashboard_returns_502() {
    let (server, _db) = test_server_with_db_config(config_with_upstream("127.0.0.1:1")).await;

    let resp = server.get("/").add_header(HOST, "l8bin.localhost").await;

    resp.assert_status(StatusCode::BAD_GATEWAY);
}

/// The fallback must stream, not buffer: the first upstream chunk arrives before the body completes.
#[tokio::test]
async fn dashboard_proxy_streams_response() {
    let stub = spawn_stub_dashboard().await;
    let (state, _db) = super::helpers::build_test_state(config_with_upstream(&stub.addr)).await;

    // Real socket + raw client — axum_test's default mock transport buffers whole bodies.
    let app = super::helpers::build_router(state);
    let config = axum_test::TestServerConfig {
        save_cookies: true,
        transport: Some(axum_test::Transport::HttpRandomPort),
        ..axum_test::TestServerConfig::new()
    };
    let server = axum_test::TestServer::new_with_config(app, config).unwrap();

    let client = reqwest::Client::new();
    let mut resp =
        client.get(server.server_url("/stream").unwrap()).header(HOST, "l8bin.localhost").send().await.unwrap();

    let first = resp.chunk().await.unwrap().expect("first chunk before release");
    assert_eq!(first, "chunk1");

    let _ = stub.release.send(true);
    let rest = resp.text().await.unwrap();
    assert_eq!(rest, "chunk2");
}
