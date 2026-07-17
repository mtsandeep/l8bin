use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::helpers::{test_server, test_server_with_db};

async fn logged_in_server() -> axum_test::TestServer {
    let server = test_server().await;
    server
        .post("/auth/register")
        .json(&json!({"username": "stageuser", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "stageuser", "password": "pass"}))
        .await;
    server
}

async fn logged_in_server_with_db() -> (axum_test::TestServer, sqlx::SqlitePool) {
    let (server, db) = test_server_with_db().await;
    server
        .post("/auth/register")
        .json(&json!({"username": "stageuser", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "stageuser", "password": "pass"}))
        .await;
    (server, db)
}

fn cleanup_project_dir(project_id: &str) {
    let path = PathBuf::from("projects").join(project_id);
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn compose_stage_only_creates_env_and_stays_unconfigured() {
    let server = logged_in_server().await;
    let project_id = "stage-compose-1";
    cleanup_project_dir(project_id);

    server
        .post("/projects")
        .json(&json!({"id": project_id}))
        .await
        .assert_status(StatusCode::CREATED);

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
        .multipart(
            axum_test::multipart::MultipartForm::new()
                .add_text("project_id", project_id)
                .add_text("stage_only", "true")
                .add_part(
                    "compose",
                    axum_test::multipart::Part::text(compose)
                        .file_name("compose.yaml")
                        .mime_type("text/yaml"),
                ),
        )
        .await;

    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["status"], "unconfigured");
    assert_eq!(body["project_id"], project_id);

    let env_path = PathBuf::from("projects").join(project_id).join(".env");
    let compose_path = PathBuf::from("projects").join(project_id).join("compose.yaml");
    assert!(env_path.exists(), "runtime .env should be created during staging");
    assert!(compose_path.exists(), "compose.yaml should be persisted during staging");

    let project = server.get(&format!("/projects/{project_id}")).await;
    project.assert_status(StatusCode::OK);
    let project_body: Value = project.json();
    assert_eq!(project_body["status"], "unconfigured");
    assert_eq!(project_body["node_id"], "local");

    cleanup_project_dir(project_id);
}

#[tokio::test]
async fn start_unconfigured_without_staged_data_fails() {
    let server = logged_in_server().await;
    let project_id = "stage-empty-1";

    server
        .post("/projects")
        .json(&json!({"id": project_id}))
        .await
        .assert_status(StatusCode::CREATED);

    let resp = server.post(&format!("/projects/{project_id}/start")).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn single_stage_only_keeps_project_unconfigured() {
    let server = logged_in_server().await;
    let project_id = "stage-single-1";
    cleanup_project_dir(project_id);

    server
        .post("/projects")
        .json(&json!({"id": project_id}))
        .await
        .assert_status(StatusCode::CREATED);

    let resp = server
        .put("/deploy")
        .json(&json!({
            "project_id": project_id,
            "image": "nginx:alpine",
            "port": 80,
            "stage_only": true
        }))
        .await;

    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["status"], "unconfigured");

    let env_path = PathBuf::from("projects").join(project_id).join(".env");
    assert!(env_path.exists(), "runtime .env should be created during single-service staging");

    let project = server.get(&format!("/projects/{project_id}")).await;
    let project_body: Value = project.json();
    assert_eq!(project_body["status"], "unconfigured");
    assert_eq!(project_body["node_id"], "local");

    cleanup_project_dir(project_id);
}

#[tokio::test]
async fn stage_only_ignored_for_already_configured_project() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "stage-redeploy-1";
    cleanup_project_dir(project_id);

    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind("stageuser")
        .fetch_one(&db)
        .await
        .unwrap();

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, node_id, created_at, updated_at)
           VALUES (?, ?, 'old:latest', 8080, 'stopped', 'local', ?, ?)"#,
    )
    .bind(project_id)
    .bind(&user_id)
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();

    let resp = server
        .put("/deploy")
        .json(&json!({
            "project_id": project_id,
            "image": "nginx:alpine",
            "port": 80,
            "stage_only": true
        }))
        .await;

    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    // Redeploys must not pause for env configuration.
    assert_eq!(body["status"], "deploying");

    cleanup_project_dir(project_id);
}
