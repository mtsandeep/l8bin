//! Auth matrix for the token scope ladder: read < deploy < manage < admin,
//! plus project-binding confinement. Auth passed vs rejected is distinguished
//! by whether the handler runs (404 for a missing project) vs the guard
//! rejecting (401/403) — the endpoints used here never touch Docker.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::helpers::test_server;

async fn login(server: &axum_test::TestServer, username: &str) {
    server.post("/auth/register").json(&json!({"username": username, "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": username, "password": "pass"})).await;
}

/// Create a token via the session API and return the plaintext Bearer value.
/// Drops the session cookie afterwards so every following request on this
/// server authenticates via the Bearer token only.
async fn create_token(server: &mut axum_test::TestServer, body: Value) -> String {
    let resp = server.post("/deploy-tokens").json(&body).await;
    resp.assert_status(StatusCode::CREATED);
    let token = resp.json::<Value>()["token"].as_str().expect("token in response").to_string();
    server.clear_cookies();
    token
}

#[tokio::test]
async fn meta_requires_authentication() {
    let server = test_server().await;
    server.get("/meta").await.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn read_token_reads_but_cannot_manage_or_deploy() {
    let mut server = test_server().await;
    login(&server, "reader").await;
    let token = create_token(&mut server, json!({"name": "t-read", "scope": "read"})).await;

    // Read group: allowed
    let resp = server.get("/meta").add_header("authorization", format!("Bearer {token}")).await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert!(body["domain"].as_str().is_some(), "meta must expose the platform domain");
    assert!(body["version"].as_str().is_some(), "meta must expose the version");

    server.get("/projects").add_header("authorization", format!("Bearer {token}")).await.assert_status(StatusCode::OK);

    // Auth passes (handler runs → 404 for the missing project), scope is enough for read
    server
        .get("/projects/missing/stats")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // Manage group: scope insufficient
    server
        .patch("/projects/missing/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({"name": "x"}))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // Admin group: scope insufficient
    server
        .get("/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // Deploy endpoints: read tokens must not be able to deploy
    server
        .post("/deploy")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({"project_id": "missing", "image": "nginx:alpine"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn manage_token_manages_but_cannot_admin() {
    let mut server = test_server().await;
    login(&server, "manager").await;
    let token = create_token(&mut server, json!({"name": "t-manage", "scope": "manage"})).await;

    let auth = format!("Bearer {token}");

    // Read still allowed (cumulative)
    server.get("/meta").add_header("authorization", auth.clone()).await.assert_status(StatusCode::OK);

    // Manage allowed: guard passes, handler 404s on the missing project
    server
        .patch("/projects/missing/settings")
        .add_header("authorization", auth.clone())
        .json(&json!({"name": "x"}))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // Admin denied
    server.get("/settings").add_header("authorization", auth.clone()).await.assert_status(StatusCode::FORBIDDEN);
    server.delete("/projects/missing").add_header("authorization", auth).await.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_token_accesses_platform_settings() {
    let mut server = test_server().await;
    login(&server, "root").await;
    let token = create_token(&mut server, json!({"name": "t-admin", "scope": "admin"})).await;

    let auth = format!("Bearer {token}");

    server.get("/settings").add_header("authorization", auth.clone()).await.assert_status(StatusCode::OK);
    server.get("/meta").add_header("authorization", auth).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn default_token_scope_is_deploy_and_can_read() {
    let mut server = test_server().await;
    login(&server, "cicd").await;

    // No scope in the request body → deploy (back-compat with the dashboard)
    let resp = server.post("/deploy-tokens").json(&json!({"name": "ci"})).await;
    resp.assert_status(StatusCode::CREATED);
    let body: Value = resp.json();
    assert_eq!(body["token_info"]["scope"], "deploy", "default scope must be deploy");

    let token = body["token"].as_str().unwrap().to_string();
    server.clear_cookies();
    let auth = format!("Bearer {token}");

    // deploy ⊇ read: the deploy-verify loop must work with the same token
    server
        .get("/projects/missing/stats")
        .add_header("authorization", auth.clone())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server
        .patch("/projects/missing/settings")
        .add_header("authorization", auth)
        .json(&json!({"name": "x"}))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn project_scoped_token_confined_to_its_project() {
    let mut server = test_server().await;
    login(&server, "scoped").await;
    server.post("/projects").json(&json!({"id": "web"})).await.assert_status(StatusCode::CREATED);

    let token = create_token(&mut server, json!({"name": "t-web", "scope": "manage", "project_id": "web"})).await;
    let auth = format!("Bearer {token}");

    // Own project subtree: guard passes, handler answers (project exists)
    server.get("/projects/web").add_header("authorization", auth.clone()).await.assert_status(StatusCode::OK);
    // /meta is allowed for project tokens (needed for URL computation)
    server.get("/meta").add_header("authorization", auth.clone()).await.assert_status(StatusCode::OK);

    // Another project's subtree: guard rejects
    server
        .get("/projects/other/stats")
        .add_header("authorization", auth.clone())
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // Cross-project views: global tokens only
    server.get("/projects").add_header("authorization", auth.clone()).await.assert_status(StatusCode::FORBIDDEN);
    server.get("/nodes").add_header("authorization", auth.clone()).await.assert_status(StatusCode::FORBIDDEN);
    server.get("/settings").add_header("authorization", auth).await.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let mut server = test_server().await;
    login(&server, "expired-owner").await;

    let expires_at = chrono::Utc::now().timestamp() - 60;
    let token =
        create_token(&mut server, json!({"name": "t-expired", "scope": "admin", "expires_at": expires_at})).await;

    server
        .get("/meta")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_management_stays_session_only() {
    let mut server = test_server().await;
    login(&server, "mint").await;
    let token = create_token(&mut server, json!({"name": "t-admin2", "scope": "admin"})).await;

    // Even an admin token must not be able to mint further tokens
    server
        .post("/deploy-tokens")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({"name": "child", "scope": "admin"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn garbage_bearer_token_is_unauthorized() {
    let server = test_server().await;
    server
        .get("/meta")
        .add_header("authorization", "Bearer not-a-real-token")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
