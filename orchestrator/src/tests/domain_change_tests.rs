use axum::http::StatusCode;
use sqlx::SqlitePool;

use crate::tests::helpers::test_server_with_db;

async fn seed(db: &SqlitePool) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u1', 'x', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT OR IGNORE INTO nodes (id, name, host, created_at, updated_at) VALUES ('local', 'local', '127.0.0.1', ?, ?)")
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, user_id, name, status, node_id, deploy_type, is_background, \
         auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports, \
         allow_docker_access, created_at, updated_at) \
         VALUES ('bg', 'u1', 'BG', 'stopped', 'local', 'compose', 1, 1, 15, 0, 0, 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

async fn login(server: &axum_test::TestServer) {
    server
        .post("/auth/register")
        .json(&serde_json::json!({"username": "tester", "password": "test-password-123"}))
        .await;
    server.post("/auth/login").json(&serde_json::json!({"username": "tester", "password": "test-password-123"})).await;
}

async fn ask(server: &axum_test::TestServer, domain: &str) -> axum_test::TestResponse {
    server.get(&format!("/caddy/ask?domain={domain}")).await
}

#[tokio::test]
async fn ask_approves_staged_and_previous_domains() {
    let (server, db) = test_server_with_db().await;
    seed(&db).await;

    // Test platform domain is "localhost": current subdomain approved, unknown denied
    ask(&server, "bg.localhost").await.assert_status(StatusCode::OK);
    ask(&server, "nope.localhost").await.assert_status(StatusCode::NOT_FOUND);

    // Staged domain approved while a job prepares it
    sqlx::query("INSERT INTO settings (key, value) VALUES ('staged_domain', 'new.example.io')")
        .execute(&db)
        .await
        .unwrap();
    ask(&server, "bg.new.example.io").await.assert_status(StatusCode::OK);
    ask(&server, "l8bin.new.example.io").await.assert_status(StatusCode::OK);

    // The apex serves the landing page and is TLS-approved
    ask(&server, "localhost").await.assert_status(StatusCode::OK);

    // Previous domain stays approved during the grace window
    sqlx::query("INSERT INTO settings (key, value) VALUES ('previous_domain', 'old.example.net')")
        .execute(&db)
        .await
        .unwrap();
    ask(&server, "bg.old.example.net").await.assert_status(StatusCode::OK);
    ask(&server, "poke.old.example.net").await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn resolve_all_routes_duplicates_previous_domain() {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();
    seed(&db).await;
    sqlx::query("INSERT INTO settings (key, value) VALUES ('previous_domain', 'old.example.net')")
        .execute(&db)
        .await
        .unwrap();

    let routes =
        crate::routing_helpers::resolve_all_routes(&db, "example.com", "litebin-orchestrator:5080").await.unwrap();
    assert!(routes.iter().any(|r| r.subdomain_host == "bg.example.com"), "current-domain route missing");
    assert!(routes.iter().any(|r| r.subdomain_host == "bg.old.example.net"), "previous-domain route missing");
}

#[tokio::test]
async fn retire_previous_drops_domain_and_records() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    seed(&db).await;
    sqlx::query("INSERT INTO settings (key, value) VALUES ('previous_domain', 'old.example.net')")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('previous_domain_retire_at', '1')")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO dns_records (name, record_id, zone_id) VALUES ('bg.old.example.net', 'rec1', '')")
        .execute(&db)
        .await
        .unwrap();

    server.post("/settings/domain/retire-previous").await.assert_status(StatusCode::OK);

    let prev: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'previous_domain'")
        .fetch_optional(&db)
        .await
        .unwrap();
    assert!(prev.is_none(), "previous_domain setting must be cleared");
    let ledger: Option<String> = sqlx::query_scalar("SELECT name FROM dns_records WHERE name = 'bg.old.example.net'")
        .fetch_optional(&db)
        .await
        .unwrap();
    assert!(ledger.is_some(), "DNS records are never auto-deleted on retirement");
}

#[tokio::test]
async fn sslip_preview_uses_dns_target() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    seed(&db).await;
    sqlx::query("INSERT INTO settings (key, value) VALUES ('dns_target', '203.0.113.5')").execute(&db).await.unwrap();

    let resp = server.get("/settings/domain/sslip").await;
    assert_eq!(resp.status_code(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&resp.text()).unwrap();
    assert_eq!(body["domain"].as_str().unwrap(), "203-0-113-5.sslip.io");
}
