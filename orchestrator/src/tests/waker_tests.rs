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
async fn insert_running_project(
    db: &sqlx::SqlitePool,
    project_id: &str,
    auto_start_enabled: bool,
) {
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

    let resp = server
        .get("/")
        .add_header(axum::http::header::HOST, "my-proj.localhost")
        .await;

    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    // Status must remain "stopped"
    let row: (String,) = sqlx::query_as("SELECT status FROM projects WHERE id = ?")
        .bind("my-proj")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(row.0, "stopped");
}

/// Requirement 5.2 — stopped project with auto_start_enabled=true does NOT return 503.
///
/// The waker passes the auto_start check and spawns a background wake.
/// It returns the loading page (200) immediately, proving the 503 auto-start gate was passed.
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

    // Must NOT be 503 (auto-start disabled). Returns 200 loading page (background wake).
    assert_ne!(
        resp.status_code(),
        StatusCode::SERVICE_UNAVAILABLE,
        "waker should not return 503 when auto_start_enabled=true"
    );
    resp.assert_status(StatusCode::OK);
}

/// Requirement 5.3 — running project with auto_start_enabled=false returns 200.
///
/// A running project with mapped_port is recognized as running, skips the auto_start
/// check entirely, syncs Caddy routes, and returns the 200 loading page.
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

    resp.assert_status(StatusCode::OK);
}

#[cfg(test)]
mod prop_tests {
    use proptest::prelude::*;
    use axum::http::StatusCode;

    use super::{seed_user, insert_project_with_status};
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
