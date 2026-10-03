//! Env API: masked reads, merge/replace, scope enforcement. Values must
//! never appear in any response body.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::helpers::test_server;

/// Unique per test: parallel tests share the `projects/` dir on disk.
fn project_id(test: &str) -> String {
    format!("envtest-{test}-{}", uuid::Uuid::new_v4().simple())
}

async fn setup(server: &axum_test::TestServer, project: &str) {
    server.post("/projects").json(&json!({"id": project})).await.assert_status(StatusCode::CREATED);
}

fn cleanup(project: &str) {
    let _ = std::fs::remove_dir_all(std::path::Path::new("projects").join(project));
}

async fn login(server: &axum_test::TestServer, username: &str) {
    server.post("/auth/register").json(&json!({"username": username, "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": username, "password": "pass"})).await;
}

/// Minted via session; callers `clear_cookies()` for Bearer-only requests.
async fn create_token(server: &axum_test::TestServer, body: Value) -> String {
    let resp = server.post("/deploy-tokens").json(&body).await;
    resp.assert_status(StatusCode::CREATED);
    resp.json::<Value>()["token"].as_str().expect("token").to_string()
}

#[tokio::test]
async fn put_then_get_masks_values() {
    let server = test_server().await;
    login(&server, "env-owner").await;
    let project = project_id("mask");
    setup(&server, &project).await;

    // Write-only update
    let resp = server
        .put(&format!("/projects/{project}/env"))
        .json(&json!({"env": {"DATABASE_URL": "postgres://user:secret-pass@db:5432/app", "MODE": "prod"}}))
        .await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert!(!body.to_string().contains("secret-pass"), "response must never contain values");
    assert_eq!(body["pending_apply"], true, "changes not yet applied to a container");

    // File on disk contains the real values (quoted, dotenvy-parseable)
    let raw = std::fs::read_to_string(std::path::Path::new("projects").join(&project).join(".env")).unwrap();
    assert!(raw.contains("DATABASE_URL=\"postgres://user:secret-pass@db:5432/app\""), "raw file: {raw}");
    assert!(raw.contains("MODE=\"prod\""));

    // Masked read-back
    let resp = server.get(&format!("/projects/{project}/env")).await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    let body_str = body.to_string();
    assert!(!body_str.contains("secret-pass"), "GET must never contain values");
    let vars: Vec<&Value> = body["vars"].as_array().unwrap().iter().collect();
    assert_eq!(vars.len(), 2);
    assert_eq!(body["pending_apply"], true);

    cleanup(&project);
}

#[tokio::test]
async fn merge_replaces_and_deletes_without_losing_neighbors() {
    let server = test_server().await;
    login(&server, "env-merger").await;
    let project = project_id("merge");
    setup(&server, &project).await;

    server
        .put(&format!("/projects/{project}/env"))
        .json(&json!({"env": {"A": "1", "B": "2", "C": "3"}}))
        .await
        .assert_status(StatusCode::OK);

    // Overwrite B, delete C, keep A untouched
    server
        .put(&format!("/projects/{project}/env"))
        .json(&json!({"env": {"B": "two"}, "delete": ["C"]}))
        .await
        .assert_status(StatusCode::OK);

    let body: Value = server.get(&format!("/projects/{project}/env")).await.json();
    let keys: Vec<&str> = body["vars"].as_array().unwrap().iter().map(|v| v["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["A", "B"]);

    let raw = std::fs::read_to_string(std::path::Path::new("projects").join(&project).join(".env")).unwrap();
    assert!(raw.contains("A=\"1\"") && raw.contains("B=\"two\"") && !raw.contains("C="));

    cleanup(&project);
}

#[tokio::test]
async fn replace_mode_resets_file_to_exactly_the_sets() {
    let server = test_server().await;
    login(&server, "env-replacer").await;
    let project = project_id("replace");
    setup(&server, &project).await;

    server
        .put(&format!("/projects/{project}/env"))
        .json(&json!({"env": {"OLD": "1"}}))
        .await
        .assert_status(StatusCode::OK);
    server
        .put(&format!("/projects/{project}/env"))
        .json(&json!({"env": {"NEW": "2"}, "mode": "replace"}))
        .await
        .assert_status(StatusCode::OK);

    let body: Value = server.get(&format!("/projects/{project}/env")).await.json();
    let keys: Vec<&str> = body["vars"].as_array().unwrap().iter().map(|v| v["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["NEW"]);

    cleanup(&project);
}

#[tokio::test]
async fn validation_rejects_bad_input() {
    let server = test_server().await;
    login(&server, "env-validator").await;
    let project = project_id("valid");
    setup(&server, &project).await;

    let url = format!("/projects/{project}/env");
    server.put(&url).json(&json!({})).await.assert_status(StatusCode::BAD_REQUEST);
    server.put(&url).json(&json!({"env": {"BAD-KEY": "x"}})).await.assert_status(StatusCode::BAD_REQUEST);
    server.put(&url).json(&json!({"env": {"1STARTS_WITH_NUM": "x"}})).await.assert_status(StatusCode::BAD_REQUEST);
    server.put(&url).json(&json!({"env": {"MULTI": "line\nbreak"}})).await.assert_status(StatusCode::BAD_REQUEST);
    server
        .put("/projects/missing-app/env")
        .json(&json!({"env": {"A": "1"}}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server.get("/projects/missing-app/env").await.assert_status(StatusCode::NOT_FOUND);

    cleanup(&project);
}

#[tokio::test]
async fn env_scopes_read_vs_manage() {
    let mut server = test_server().await;
    login(&server, "env-scoped").await;
    let project = project_id("scope");
    setup(&server, &project).await;

    let read_tok = create_token(&server, json!({"name": "env-read", "scope": "read", "project_id": project})).await;
    let manage_tok =
        create_token(&server, json!({"name": "env-manage", "scope": "manage", "project_id": project})).await;
    server.clear_cookies();

    // Read token: masked GET allowed, PUT forbidden
    server
        .get(&format!("/projects/{project}/env"))
        .add_header("authorization", format!("Bearer {read_tok}"))
        .await
        .assert_status(StatusCode::OK);
    server
        .put(&format!("/projects/{project}/env"))
        .add_header("authorization", format!("Bearer {read_tok}"))
        .json(&json!({"env": {"A": "1"}}))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // Manage token (project-scoped): write allowed on its own project
    server
        .put(&format!("/projects/{project}/env"))
        .add_header("authorization", format!("Bearer {manage_tok}"))
        .json(&json!({"env": {"A": "1"}}))
        .await
        .assert_status(StatusCode::OK);

    cleanup(&project);
}
