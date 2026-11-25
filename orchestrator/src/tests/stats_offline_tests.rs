//! Stats-poll resilience: an offline agent node must not stall `/projects/stats`.
//! Its projects are served from DB state and the response returns fast.

use axum::http::StatusCode;
use axum_test::TestServer;
use sqlx::SqlitePool;

use crate::tests::helpers::test_server_with_db;

async fn login(server: &TestServer) {
    server
        .post("/auth/register")
        .json(&serde_json::json!({"username": "tester", "password": "test-password-123"}))
        .await;
    server.post("/auth/login").json(&serde_json::json!({"username": "tester", "password": "test-password-123"})).await;
}

async fn insert_dead_node(db: &SqlitePool) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO nodes (id, name, host, public_ip, agent_port, status, fail_count, created_at, updated_at)
         VALUES ('remote-dead', 'dead', '10.9.9.9', '10.9.9.9', 5083, 'offline', 9, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

/// Remote running project on the offline node, with a live container ID.
async fn insert_remote_project(db: &SqlitePool) {
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(db)
        .await
        .expect("login first so a user row exists");
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects (id, user_id, status, node_id, deploy_type, is_background, \
         auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports, \
         allow_docker_access, created_at, updated_at) \
         VALUES ('remote-app', ?, 'running', 'remote-dead', 'compose', 0, 1, 15, 0, 0, 0, ?, ?)",
    )
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_services (project_id, service_name, image, container_id, status) VALUES (?, ?, ?, ?, 'running')",
    )
    .bind("remote-app")
    .bind("web")
    .bind("sha256:abc")
    .bind("deadcontainer123")
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn stats_skips_offline_node_and_returns_fast() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_dead_node(&db).await;
    insert_remote_project(&db).await;

    let started = std::time::Instant::now();
    let res = server.get("/projects/stats").await;
    let elapsed = started.elapsed();

    assert_eq!(res.status_code(), StatusCode::OK);
    // Timing out here would mean per-call timeouts against offline nodes.
    assert!(elapsed < std::time::Duration::from_secs(5), "stats took {:?} with an offline node", elapsed);

    let body: serde_json::Value = res.json();
    let stats = body["stats"].as_array().expect("stats array");
    let remote = stats.iter().find(|s| s["project_id"] == "remote-app").expect("remote project in response");
    assert_eq!(remote["status"], "running", "offline-node project keeps its DB status");
}

/// Two requests in a row: the coalescing path serves the second from cache
/// without recomputing. Both must succeed.
#[tokio::test]
async fn stats_second_poll_served_from_cache() {
    let (server, _db) = test_server_with_db().await;
    login(&server).await;
    insert_dead_node(&_db).await;
    insert_remote_project(&_db).await;

    let first = server.get("/projects/stats").await;
    assert_eq!(first.status_code(), StatusCode::OK);

    let second = server.get("/projects/stats").await;
    assert_eq!(second.status_code(), StatusCode::OK);
    let body: serde_json::Value = second.json();
    assert!(body["stats"].as_array().is_some());
}
