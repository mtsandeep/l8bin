//! Backup endpoint tests. The test AppState uses an in-memory DB and an unreachable
//! Docker stub, so backup reports unavailable and enable/s3/restore degrade cleanly.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::helpers::test_server;

async fn admin_server() -> (axum_test::TestServer, String) {
    let mut server = test_server().await;
    server.post("/auth/register").json(&json!({"username": "root", "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": "root", "password": "pass"})).await;
    let resp = server.post("/deploy-tokens").json(&json!({"name": "t-admin", "scope": "admin"})).await;
    let token = resp.json::<Value>()["token"].as_str().expect("token").to_string();
    server.clear_cookies();
    (server, format!("Bearer {token}"))
}

#[tokio::test]
async fn backup_status_requires_admin() {
    let server = test_server().await;
    server.get("/backup/status").await.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn backup_status_reports_unavailable_on_memory_db() {
    let (server, auth) = admin_server().await;
    let resp = server.get("/backup/status").add_header("authorization", auth).await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["enabled"], false);
    assert_eq!(body["available"], false, "in-memory db cannot share storage with a sidecar");
    assert_eq!(body["sidecar_installed"], false);
    assert_eq!(body["sidecar_running"], false);
    assert_eq!(body["s3"]["configured"], false);
    assert!(body["last_sync"].is_null());
}

#[tokio::test]
async fn backup_enable_conflicts_on_memory_db() {
    let (server, auth) = admin_server().await;
    server.post("/backup/enable").add_header("authorization", auth.clone()).await.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn backup_s3_requires_backup_enabled() {
    let (server, auth) = admin_server().await;
    let resp = server
        .post("/backup/s3")
        .add_header("authorization", auth)
        .json(&json!({
            "endpoint": "https://acct.r2.cloudflarestorage.com",
            "bucket": "buck",
            "access_key_id": "AKIA",
            "secret_access_key": "s3cr3t"
        }))
        .await;
    resp.assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn backup_s3_validates_fields() {
    let (server, auth) = admin_server().await;
    let resp = server
        .post("/backup/s3")
        .add_header("authorization", auth)
        .json(&json!({"endpoint": "", "bucket": "", "access_key_id": "", "secret_access_key": ""}))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn backup_restore_conflicts_without_sidecar() {
    let (server, auth) = admin_server().await;
    server
        .post("/backup/restore")
        .add_header("authorization", auth)
        .json(&json!({}))
        .await
        .assert_status(StatusCode::CONFLICT);
}
