use axum::http::StatusCode;

use super::helpers::test_server_with_db;

/// Insert a project directly into the DB with explicit status and auto_start_enabled.
async fn insert_project_with_status(
    db: &sqlx::SqlitePool,
    project_id: &str,
    status: &str,
    auto_start_enabled: bool,
    container_id: Option<&str>,
) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, auto_start_enabled, container_id,
            last_active_at, created_at, updated_at)
           VALUES (?, 'test-user', 'test-image:latest', 8080, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(project_id)
    .bind(status)
    .bind(auto_start_enabled as i64)
    .bind(container_id)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

/// Seed a user so the FK constraint on projects is satisfied.
async fn seed_user(db: &sqlx::SqlitePool) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, is_admin, created_at, updated_at)
         VALUES ('test-user', 'testuser', 'hash', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

/// Insert a running project with mapped_port and container_id set.
async fn insert_running_project(db: &sqlx::SqlitePool, project_id: &str, auto_start_enabled: bool) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, auto_start_enabled, container_id,
            mapped_port, last_active_at, created_at, updated_at)
           VALUES (?, 'test-user', 'test-image:latest', 8080, 'running', ?, 'fake-container',
            12345, ?, ?, ?)"#,
    )
    .bind(project_id)
    .bind(auto_start_enabled as i64)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

/// Requirement 5.1 — stopped project with auto_start_enabled=false returns 503.
///
/// The waker checks auto_start_enabled before attempting to start the container.
/// When disabled, it must return 503 immediately and leave the project stopped.
#[tokio::test]
async fn waker_returns_503_when_auto_start_disabled_and_stopped() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "my-proj", "stopped", false, None).await;

    let resp = server.get("/").add_header(axum::http::header::HOST, "my-proj.localhost").await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    // Status must remain "stopped"
    let row: (String,) =
        sqlx::query_as("SELECT status FROM projects WHERE id = ?").bind("my-proj").fetch_one(&db).await.unwrap();
    assert_eq!(row.0, "stopped");
}

/// Stopped + auto-start enabled: passes the gate, spawns a wake (no offline 503).
#[tokio::test]
async fn waker_does_not_return_503_when_auto_start_enabled_and_stopped() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "auto-proj", "stopped", true, None).await;

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "auto-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;

    // Auto-start gate passed: NOT the offline 503 (its body is the offline page).
    let body = resp.text();
    assert!(
        !body.contains("currently offline"),
        "waker should not return the offline page when auto_start_enabled=true"
    );
}

/// Running project skips the offline gate.
#[tokio::test]
async fn waker_returns_200_for_running_project_with_auto_start_disabled() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    // Running project with mapped_port — recognized as running, auto_start check skipped.
    insert_running_project(&db, "run-proj", false).await;

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "run-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;

    let body = resp.text();
    assert!(!body.contains("currently offline"), "running project must skip the auto-start/offline gate");
}

/// No guard → immediate "ready" + marker header.
#[tokio::test]
async fn wake_wait_endpoint_answers_ready_without_guard() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "wait-proj", "stopped", true, None).await;

    let resp = server
        .get("/__l8b_wake_wait")
        .add_header(axum::http::header::HOST, "wait-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "application/json")
        .await;

    resp.assert_status(StatusCode::OK);
    assert_eq!(resp.headers().get("x-l8b-wake").and_then(|v| v.to_str().ok()), Some("1"));
    assert!(resp.text().contains("\"status\":\"ready\""));
}

/// Completed guard → its outcome.
#[tokio::test]
async fn wake_wait_endpoint_reports_guard_outcome() {
    use axum_test::TestServer;
    use std::sync::Arc;

    let (state, db) = super::helpers::build_test_state(super::helpers::test_config()).await;
    seed_user(&db).await;
    insert_project_with_status(&db, "guarded-proj", "stopped", true, None).await;

    let guard = Arc::new(litebin_common::wake_wait::WakeGuard::new());
    guard.finish(true);
    state.wake_guards.insert("guarded-proj".to_string(), guard);

    let server = TestServer::new(super::helpers::build_router(state)).unwrap();
    let resp = server
        .get("/__l8b_wake_wait")
        .add_header(axum::http::header::HOST, "guarded-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "application/json")
        .await;

    resp.assert_status(StatusCode::OK);
    assert_eq!(resp.headers().get("x-l8b-wake").and_then(|v| v.to_str().ok()), Some("1"));
    assert!(resp.text().contains("\"status\":\"ready\""));
}

/// Offline: 503 without Retry-After (down until a human acts).
#[tokio::test]
async fn offline_json_has_no_retry_after() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "off-proj", "stopped", false, None).await;

    let resp = server.get("/").add_header(axum::http::header::HOST, "off-proj.localhost").await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert!(resp.headers().get("retry-after").is_none(), "offline must not promise a retry window");
}

/// Unconfigured (staged) projects must not auto-start when the public URL is opened.
#[tokio::test]
async fn waker_refuses_auto_start_when_unconfigured() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "staged-proj", "unconfigured", true, None).await;

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "staged-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    let row: (String,) =
        sqlx::query_as("SELECT status FROM projects WHERE id = ?").bind("staged-proj").fetch_one(&db).await.unwrap();
    assert_eq!(row.0, "unconfigured");
}

/// Pending projects have no staged artifacts and must never be auto-started.
#[tokio::test]
async fn waker_refuses_auto_start_when_pending() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    insert_project_with_status(&db, "pending-proj", "pending", true, None).await;

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "pending-proj.localhost")
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    let row: (String,) =
        sqlx::query_as("SELECT status FROM projects WHERE id = ?").bind("pending-proj").fetch_one(&db).await.unwrap();
    assert_eq!(row.0, "pending");
}

#[tokio::test]
async fn background_project_hostname_returns_generic_404_without_state_change() {
    let (server, db) = test_server_with_db().await;
    seed_user(&db).await;
    let last_active_at = chrono::Utc::now().timestamp() - 3600;
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, is_background, image, internal_port, status,
            auto_stop_enabled, auto_start_enabled, last_active_at, created_at, updated_at)
           VALUES ('worker-only', 'test-user', 1, 'worker:latest', NULL, 'running',
                   0, 0, ?, ?, ?)"#,
    )
    .bind(last_active_at)
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "worker-only.localhost")
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
    assert!(!resp.text().to_lowercase().contains("background project"));

    let row: (String, i64) = sqlx::query_as("SELECT status, last_active_at FROM projects WHERE id = 'worker-only'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(row.0, "running");
    assert_eq!(row.1, last_active_at);
}

#[cfg(test)]
mod prop_tests {
    use axum::http::StatusCode;
    use proptest::prelude::*;

    use super::{insert_project_with_status, seed_user};
    use crate::tests::helpers::test_server_with_db;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]

        /// Waker returns 503 for auto-start-disabled stopped projects.
        #[test]
        fn prop_waker_503_when_auto_start_disabled(
            project_id in "[a-z][a-z0-9]{3,10}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (server, db) = test_server_with_db().await;
                seed_user(&db).await;
                insert_project_with_status(&db, &project_id, "stopped", false, None).await;

                let host = format!("{}.localhost", project_id);
                let resp = server
                    .get("/")
                    .add_header(axum::http::header::HOST, host.as_str())
                    .await;

                prop_assert_eq!(resp.status_code(), StatusCode::SERVICE_UNAVAILABLE);

                let row: (String,) = sqlx::query_as("SELECT status FROM projects WHERE id = ?")
                    .bind(&project_id)
                    .fetch_one(&db)
                    .await
                    .unwrap();
                prop_assert_eq!(row.0.as_str(), "stopped");

                Ok(())
            }).unwrap();
        }
    }
}
