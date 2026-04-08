use axum::http::StatusCode;
use serde_json::{json, Value};

use super::helpers::test_server;

#[tokio::test]
async fn register_creates_user() {
    let server = test_server().await;

    let resp = server
        .post("/auth/register")
        .json(&json!({"username": "alice", "password": "secret123"}))
        .await;

    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["user"]["username"], "alice");
    assert_eq!(body["user"]["is_admin"], true);
}

/// Register is initial-setup-only; after the first user is created,
/// further registrations are forbidden (403).
#[tokio::test]
async fn register_duplicate_username_returns_forbidden() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "bob", "password": "pass"}))
        .await
        .assert_status(StatusCode::OK);

    server
        .post("/auth/register")
        .json(&json!({"username": "bob", "password": "other"}))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_with_valid_credentials_returns_user() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "carol", "password": "mypass"}))
        .await;

    let resp = server
        .post("/auth/login")
        .json(&json!({"username": "carol", "password": "mypass"}))
        .await;

    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["user"]["username"], "carol");
}

#[tokio::test]
async fn login_with_wrong_password_returns_401() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "dave", "password": "correct"}))
        .await;

    server
        .post("/auth/login")
        .json(&json!({"username": "dave", "password": "wrong"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_without_session_returns_401() {
    let server = test_server().await;
    server.get("/auth/me").await.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_after_login_returns_user() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "eve", "password": "pass"}))
        .await;

    server
        .post("/auth/login")
        .json(&json!({"username": "eve", "password": "pass"}))
        .await;

    let resp = server.get("/auth/me").await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert_eq!(body["username"], "eve");
}

#[tokio::test]
async fn logout_clears_session() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "frank", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "frank", "password": "pass"}))
        .await;

    server.post("/auth/logout").await.assert_status(StatusCode::OK);
    server.get("/auth/me").await.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn change_password_works() {
    let server = test_server().await;

    server
        .post("/auth/register")
        .json(&json!({"username": "grace", "password": "oldpass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "grace", "password": "oldpass"}))
        .await;

    server
        .post("/auth/change-password")
        .json(&json!({"current_password": "oldpass", "new_password": "newpass"}))
        .await
        .assert_status(StatusCode::OK);

    // Old password no longer works
    server
        .post("/auth/login")
        .json(&json!({"username": "grace", "password": "oldpass"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // New password works
    server
        .post("/auth/login")
        .json(&json!({"username": "grace", "password": "newpass"}))
        .await
        .assert_status(StatusCode::OK);
}
