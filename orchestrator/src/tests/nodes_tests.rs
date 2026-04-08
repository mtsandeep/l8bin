use axum::http::StatusCode;
use serde_json::{json, Value};

use super::helpers::test_server;

async fn logged_in_server() -> axum_test::TestServer {
    let server = test_server().await;
    server
        .post("/auth/register")
        .json(&json!({"username": "admin", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "admin", "password": "pass"}))
        .await;
    server
}

#[tokio::test]
async fn list_nodes_returns_local_node() {
    let server = logged_in_server().await;
    let resp = server.get("/nodes").await;
    resp.assert_status(StatusCode::OK);
    let nodes: Value = resp.json();
    let arr = nodes.as_array().unwrap();
    assert!(!arr.is_empty(), "should have at least the local node");
    assert!(arr.iter().any(|n| n["id"] == "local"));
}

#[tokio::test]
async fn list_nodes_requires_auth() {
    let server = test_server().await;
    server
        .get("/nodes")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_local_node_returns_400() {
    let server = logged_in_server().await;
    server
        .delete("/nodes/local")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_nonexistent_node_returns_204() {
    // Deleting a node that doesn't exist is idempotent — no rows deleted, still 204
    let server = logged_in_server().await;
    server
        .delete("/nodes/ghost-node")
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn create_node_returns_pending_setup() {
    // create_node no longer does health check — returns pending_setup immediately
    let server = logged_in_server().await;
    let resp = server
        .post("/nodes")
        .json(&json!({
            "name": "test-worker",
            "host": "127.0.0.1",
            "agent_port": 19999
        }))
        .await;
    resp.assert_status(StatusCode::CREATED);
    let body: Value = resp.json();
    assert_eq!(body["status"], "pending_setup");
    assert_eq!(body["name"], "test-worker");
    // agent_secret is shown only at creation
    assert!(body["agent_secret"].as_str().unwrap().len() > 0);
}
