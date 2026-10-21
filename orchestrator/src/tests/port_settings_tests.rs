use axum::http::StatusCode;
use serde_json::json;

use super::helpers::test_server_with_db;

async fn logged_in_server_with_db() -> (axum_test::TestServer, sqlx::SqlitePool) {
    let (server, db) = test_server_with_db().await;
    server.post("/auth/register").json(&json!({"username": "portuser", "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": "portuser", "password": "pass"})).await;
    (server, db)
}

async fn insert_project(db: &sqlx::SqlitePool, project_id: &str, deploy_type: &str) {
    let user_id: String =
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?").bind("portuser").fetch_one(db).await.unwrap();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, node_id, deploy_type, created_at, updated_at)
           VALUES (?, ?, 'nginx:alpine', 3000, 'running', 'local', ?, ?, ?)"#,
    )
    .bind(project_id)
    .bind(&user_id)
    .bind(deploy_type)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();

    // Mirror the single-service "web" row deploys create — route resolution
    // prefers this port over projects.internal_port.
    sqlx::query(
        r#"INSERT INTO project_services (project_id, service_name, image, port, is_public, status)
           VALUES (?, 'web', 'nginx:alpine', 3000, 1, 'running')"#,
    )
    .bind(project_id)
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn port_patch_updates_project_and_public_service_row() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "port-fix-1";
    insert_project(&db, project_id, "image").await;

    let resp = server.patch(&format!("/projects/{project_id}/settings")).json(&json!({"port": 80})).await;
    resp.assert_status(StatusCode::OK);

    let project_port: i64 = sqlx::query_scalar("SELECT internal_port FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&db)
        .await
        .unwrap();
    let svc_port: i64 = sqlx::query_scalar("SELECT port FROM project_services WHERE project_id = ? AND is_public = 1")
        .bind(project_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!((project_port, svc_port), (80, 80));
}

#[tokio::test]
async fn port_patch_rejected_for_compose_projects() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "port-compose-1";
    insert_project(&db, project_id, "compose").await;

    let resp = server.patch(&format!("/projects/{project_id}/settings")).json(&json!({"port": 80})).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn port_patch_rejects_out_of_range() {
    let (server, db) = logged_in_server_with_db().await;
    let project_id = "port-range-1";
    insert_project(&db, project_id, "image").await;

    let resp = server.patch(&format!("/projects/{project_id}/settings")).json(&json!({"port": 0})).await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    let resp = server.patch(&format!("/projects/{project_id}/settings")).json(&json!({"port": 70000})).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}
