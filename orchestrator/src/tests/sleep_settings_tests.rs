use axum::http::StatusCode;
use serde_json::json;
use std::path::PathBuf;

use super::helpers::test_server_with_db;

async fn logged_in_server_with_db() -> (axum_test::TestServer, sqlx::SqlitePool) {
    let (server, db) = test_server_with_db().await;
    server.post("/auth/register").json(&json!({"username": "sleepuser", "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": "sleepuser", "password": "pass"})).await;
    (server, db)
}

fn cleanup_project_dir(project_id: &str) {
    let _ = std::fs::remove_dir_all(PathBuf::from("projects").join(project_id));
}

async fn insert_project(db: &sqlx::SqlitePool, project_id: &str, auto_stop: bool, timeout_mins: i64, auto_start: bool) {
    let user_id: String =
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?").bind("sleepuser").fetch_one(db).await.unwrap();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, node_id, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, created_at, updated_at)
           VALUES (?, ?, 'old:latest', 8080, 'stopped', 'local', ?, ?, ?, ?, ?)"#,
    )
    .bind(project_id)
    .bind(&user_id)
    .bind(auto_stop)
    .bind(timeout_mins)
    .bind(auto_start)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

async fn stored_sleep(db: &sqlx::SqlitePool, project_id: &str) -> (bool, i64, bool) {
    sqlx::query_as("SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn first_deploy_omitting_sleep_fields_gets_platform_defaults() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "sleep-new-1";
    cleanup_project_dir(project_id);

    let resp = server
        .put("/deploy")
        .json(&json!({
            "project_id": project_id,
            "image": "nginx:alpine",
            "port": 80
        }))
        .await;

    resp.assert_status(StatusCode::OK);
    // test_config() sets default_auto_stop_mins = 15
    assert_eq!(stored_sleep(&db, project_id).await, (true, 15, true));

    cleanup_project_dir(project_id);
}

#[tokio::test]
async fn redeploy_omitting_sleep_fields_preserves_stored_settings() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "sleep-keep-1";
    cleanup_project_dir(project_id);
    insert_project(&db, project_id, false, 45, false).await;

    let resp = server
        .put("/deploy")
        .json(&json!({
            "project_id": project_id,
            "image": "nginx:alpine",
            "port": 80
        }))
        .await;

    resp.assert_status(StatusCode::OK);
    assert_eq!(stored_sleep(&db, project_id).await, (false, 45, false));

    cleanup_project_dir(project_id);
}

#[tokio::test]
async fn redeploy_explicit_timeout_overrides_only_that_field() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "sleep-partial-1";
    cleanup_project_dir(project_id);
    insert_project(&db, project_id, true, 45, true).await;

    let resp = server
        .put("/deploy")
        .json(&json!({
            "project_id": project_id,
            "image": "nginx:alpine",
            "port": 80,
            "auto_stop_timeout_mins": 5
        }))
        .await;

    resp.assert_status(StatusCode::OK);
    // Only the explicitly provided field changes; the rest keep stored values.
    assert_eq!(stored_sleep(&db, project_id).await, (true, 5, true));

    cleanup_project_dir(project_id);
}

#[tokio::test]
async fn compose_redeploy_omitting_sleep_fields_preserves_stored_settings() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "sleep-compose-1";
    cleanup_project_dir(project_id);
    insert_project(&db, project_id, false, 45, false).await;

    let compose = r#"
services:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
    labels:
      litebin.public: "true"
"#;

    let resp = server
        .post("/deploy/compose")
        .multipart(axum_test::multipart::MultipartForm::new().add_text("project_id", project_id).add_part(
            "compose",
            axum_test::multipart::Part::text(compose).file_name("compose.yaml").mime_type("text/yaml"),
        ))
        .await;

    resp.assert_status(StatusCode::OK);
    assert_eq!(stored_sleep(&db, project_id).await, (false, 45, false));

    cleanup_project_dir(project_id);
}
