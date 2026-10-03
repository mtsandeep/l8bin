//! Device pairing lifecycle: start → poll → approve (session) → claim → token
//! works; deny, expiry, and single-claim are enforced.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::helpers::{test_server, test_server_with_db};

async fn login(server: &axum_test::TestServer, username: &str) {
    server.post("/auth/register").json(&json!({"username": username, "password": "pass"})).await;
    server.post("/auth/login").json(&json!({"username": username, "password": "pass"})).await;
}

async fn start_flow(server: &axum_test::TestServer, scope: &str) -> Value {
    let resp = server.post("/auth/device/start").json(&json!({"client_name": "l8b test", "scope": scope})).await;
    resp.assert_status(StatusCode::OK);
    resp.json()
}

async fn poll(server: &axum_test::TestServer, device_code: &str) -> Value {
    server.post("/auth/device/token").json(&json!({"device_code": device_code})).await.json()
}

#[tokio::test]
async fn full_pairing_flow_issues_a_working_scoped_token() {
    let mut server = test_server().await;
    login(&server, "pairer").await;

    let start = start_flow(&server, "manage").await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();
    assert!(user_code.starts_with("L8B-"), "user code format: {user_code}");
    assert_eq!(start["expires_in"], 600);

    // Before approval: pending, and lookup shows the request
    assert_eq!(poll(&server, &device_code).await["status"], "pending");
    let info = server.get(&format!("/auth/device?user_code={user_code}")).await;
    info.assert_status(StatusCode::OK);
    let info: Value = info.json();
    assert_eq!(info["suggested_scope"], "manage");
    assert_eq!(info["client_name"], "l8b test");

    // Approve at a lower scope than suggested
    server
        .post("/auth/device/approve")
        .json(&json!({"user_code": user_code, "approve": true, "scope": "read"}))
        .await
        .assert_status(StatusCode::OK);

    // Claim: token arrives once, carries the approved scope
    let claimed = poll(&server, &device_code).await;
    assert_eq!(claimed["status"], "ok");
    let token = claimed["token"].as_str().expect("token").to_string();
    assert_eq!(claimed["scope"], "read");

    // Single claim: polling again never returns another token
    assert_eq!(poll(&server, &device_code).await["status"], "expired");

    // The paired token actually works (read) and is properly scoped (no manage)
    server.clear_cookies();
    server.get("/meta").add_header("authorization", format!("Bearer {token}")).await.assert_status(StatusCode::OK);
    server
        .patch("/projects/missing/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({"name": "x"}))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn deny_flow_returns_denied_and_no_token() {
    let server = test_server().await;
    login(&server, "denier").await;

    let start = start_flow(&server, "deploy").await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();

    server
        .post("/auth/device/approve")
        .json(&json!({"user_code": user_code, "approve": false, "scope": "deploy"}))
        .await
        .assert_status(StatusCode::OK);

    let denied = poll(&server, &device_code).await;
    assert_eq!(denied["status"], "denied");
    assert!(denied["token"].is_null());
}

#[tokio::test]
async fn project_binding_restricts_the_paired_token() {
    let mut server = test_server().await;
    login(&server, "binder").await;
    server.post("/projects").json(&json!({"id": "bound-app"})).await.assert_status(StatusCode::CREATED);

    let start = start_flow(&server, "manage").await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    server
        .post("/auth/device/approve")
        .json(&json!({"user_code": start["user_code"], "approve": true, "scope": "manage", "project_id": "bound-app"}))
        .await
        .assert_status(StatusCode::OK);

    let claimed = poll(&server, &device_code).await;
    assert_eq!(claimed["project_id"], "bound-app");

    server.clear_cookies();
    let token = claimed["token"].as_str().unwrap();
    server
        .get("/projects/bound-app")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status(StatusCode::OK);
    server
        .get("/projects/other-app/stats")
        .add_header("authorization", format!("Bearer {token}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_codes_cannot_be_approved_or_claimed() {
    let (server, db) = test_server_with_db().await;
    login(&server, "expirer").await;

    let start = start_flow(&server, "deploy").await;
    let device_code = start["device_code"].as_str().unwrap().to_string();
    let user_code = start["user_code"].as_str().unwrap().to_string();

    // Age the code past its TTL directly
    sqlx::query("UPDATE device_codes SET expires_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().timestamp() - 1)
        .bind(&device_code)
        .execute(&db)
        .await
        .unwrap();

    server
        .post("/auth/device/approve")
        .json(&json!({"user_code": user_code, "approve": true, "scope": "deploy"}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(poll(&server, &device_code).await["status"], "expired");
}

#[tokio::test]
async fn approve_and_lookup_require_a_session() {
    let server = test_server().await;

    server
        .post("/auth/device/approve")
        .json(&json!({"user_code": "L8B-XXXXXX", "approve": true, "scope": "deploy"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server.get("/auth/device?user_code=L8B-XXXXXX").await.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_codes_are_not_pending_and_not_claimable() {
    let server = test_server().await;
    login(&server, "lookup-user").await;

    server.get("/auth/device?user_code=L8B-ZZZZZZ").await.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(poll(&server, "not-a-real-uuid").await["status"], "expired");
}
