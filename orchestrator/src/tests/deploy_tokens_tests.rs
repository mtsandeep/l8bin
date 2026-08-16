use axum::http::StatusCode;
use serde_json::{Value, json};

use super::helpers::test_server;

async fn login(server: &axum_test::TestServer, username: &str) {
    server.post("/auth/register").json(&json!({"username": username, "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": username, "password": "pass"})).await;
}

#[tokio::test]
async fn token_endpoints_require_auth() {
    let server = test_server().await;

    server.post("/deploy-tokens").json(&json!({})).await.assert_status(StatusCode::UNAUTHORIZED);
    server.get("/deploy-tokens").await.assert_status(StatusCode::UNAUTHORIZED);
    server.delete("/deploy-tokens/some-id").await.assert_status(StatusCode::UNAUTHORIZED);
}

/// The raw token is only shown once at creation; the list endpoint must never
/// leak it (only the SHA-256 hash is stored).
#[tokio::test]
async fn create_global_token_returns_token_and_lists_it() {
    let server = test_server().await;
    login(&server, "token-owner").await;

    let resp = server.post("/deploy-tokens").json(&json!({"name": "ci-deploy"})).await;
    resp.assert_status(StatusCode::CREATED);

    let body: Value = resp.json();
    let token = body["token"].as_str().expect("token in response");
    assert_eq!(token.len(), 64, "token must be 64 hex chars");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(body["token_info"]["name"], "ci-deploy");

    // A 201 must mean the token was actually persisted: it shows up in the list.
    let list: Value = server.get("/deploy-tokens").await.json();
    let entries: Vec<&Value> =
        list.as_array().expect("token list").iter().filter(|t| t["id"] == body["token_info"]["id"]).collect();
    assert_eq!(entries.len(), 1, "created token must appear in list exactly once");
    assert!(entries[0].get("token").is_none(), "list must not expose the raw token");
    assert!(entries[0].get("token_hash").is_none(), "list must not expose the token hash");
}

#[tokio::test]
async fn create_token_for_unknown_project_returns_404() {
    let server = test_server().await;
    login(&server, "scoped-owner").await;

    server
        .post("/deploy-tokens")
        .json(&json!({"project_id": "does-not-exist", "name": "scoped"}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_project_scoped_token_and_filter_list() {
    let server = test_server().await;
    login(&server, "scoped-user").await;

    server.post("/projects").json(&json!({"id": "scoped-app"})).await.assert_status(StatusCode::CREATED);

    let resp = server.post("/deploy-tokens").json(&json!({"project_id": "scoped-app", "name": "app-token"})).await;
    resp.assert_status(StatusCode::CREATED);
    let body: Value = resp.json();
    assert_eq!(body["token_info"]["project_id"], "scoped-app");

    // Unfiltered list includes the scoped token; the project filter matches it too.
    let all: Value = server.get("/deploy-tokens").await.json();
    assert!(all.as_array().expect("token list").iter().any(|t| t["id"] == body["token_info"]["id"]));

    let filtered: Value = server.get("/deploy-tokens?project_id=scoped-app").await.json();
    assert!(filtered.as_array().expect("token list").iter().any(|t| t["id"] == body["token_info"]["id"]));

    let other: Value = server.get("/deploy-tokens?project_id=other-app").await.json();
    assert!(other.as_array().expect("token list").iter().all(|t| t["id"] != body["token_info"]["id"]));
}

#[tokio::test]
async fn revoke_token_returns_204_then_404() {
    let server = test_server().await;
    login(&server, "revoke-user").await;

    let body: Value = server.post("/deploy-tokens").json(&json!({"name": "to-revoke"})).await.json();
    let token_id = body["token_info"]["id"].as_str().expect("token id").to_string();

    server.delete(&format!("/deploy-tokens/{token_id}")).await.assert_status(StatusCode::NO_CONTENT);

    // Revoked token disappears from the list and cannot be revoked again.
    let list: Value = server.get("/deploy-tokens").await.json();
    assert!(list.as_array().expect("token list").iter().all(|t| t["id"] != body["token_info"]["id"]));
    server.delete(&format!("/deploy-tokens/{token_id}")).await.assert_status(StatusCode::NOT_FOUND);
}
