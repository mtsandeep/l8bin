use axum::http::StatusCode;
use serde_json::{json, Value};

use super::helpers::{test_server, test_server_with_db};

/// Register + login, return the server (session cookie is preserved by TestServer).
async fn logged_in_server() -> axum_test::TestServer {
    let server = test_server().await;
    server
        .post("/auth/register")
        .json(&json!({"username": "testuser", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "testuser", "password": "pass"}))
        .await;
    server
}

#[tokio::test]
async fn list_projects_empty_initially() {
    let server = logged_in_server().await;
    let resp = server.get("/projects").await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_projects_requires_auth() {
    let server = test_server().await;
    server
        .get("/projects")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_nonexistent_project_returns_404() {
    let server = logged_in_server().await;
    server
        .delete("/projects/does-not-exist")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_nonexistent_project_returns_404() {
    let server = logged_in_server().await;
    server
        .post("/projects/does-not-exist/stop")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_nonexistent_project_returns_404() {
    let server = logged_in_server().await;
    server
        .get("/projects/does-not-exist/stats")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn project_lifecycle_via_db() {
    // Verify the API starts with an empty project list for a fresh user.
    let server = logged_in_server().await;
    let resp = server.get("/projects").await;
    resp.assert_status(StatusCode::OK);
    assert!(resp.json::<Value>().as_array().unwrap().is_empty());
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Register + login using the server that also exposes the DB pool.
async fn logged_in_server_with_db() -> (axum_test::TestServer, sqlx::SqlitePool) {
    let (server, db) = test_server_with_db().await;
    server
        .post("/auth/register")
        .json(&json!({"username": "testuser", "password": "pass"}))
        .await;
    server
        .post("/auth/login")
        .json(&json!({"username": "testuser", "password": "pass"}))
        .await;
    (server, db)
}

/// Insert a minimal project row directly into the DB with default timeout fields.
async fn insert_project(db: &sqlx::SqlitePool, project_id: &str, user_id: &str) {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO projects
           (id, user_id, image, internal_port, status, created_at, updated_at)
           VALUES (?, ?, 'test-image:latest', 8080, 'running', ?, ?)"#,
    )
    .bind(project_id)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

/// Fetch the user_id for a logged-in session via GET /auth/me.
async fn get_user_id(server: &axum_test::TestServer) -> String {
    let resp = server.get("/auth/me").await;
    resp.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Requirements 1.4 — deploying without timeout fields yields defaults.
#[tokio::test]
async fn deploy_without_timeout_fields_yields_defaults() {
    let (server, db) = logged_in_server_with_db().await;
    let user_id = get_user_id(&server).await;

    // Insert project directly so we don't need Docker running.
    insert_project(&db, "my-app", &user_id).await;

    let resp = server.get("/projects/my-app").await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();

    assert_eq!(body["auto_stop_enabled"], json!(true));
    assert_eq!(body["auto_stop_timeout_mins"], json!(15));
    assert_eq!(body["auto_start_enabled"], json!(true));
}

/// Requirements 2.1, 2.2 — GET /projects/:id returns all three timeout fields.
#[tokio::test]
async fn get_project_returns_timeout_fields() {
    let (server, db) = logged_in_server_with_db().await;
    let user_id = get_user_id(&server).await;
    insert_project(&db, "proj-get", &user_id).await;

    let resp = server.get("/projects/proj-get").await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();

    // All three fields must be present and have the correct types.
    assert!(body.get("auto_stop_enabled").is_some());
    assert!(body.get("auto_stop_timeout_mins").is_some());
    assert!(body.get("auto_start_enabled").is_some());
    assert_eq!(body["id"], json!("proj-get"));
}

/// Requirements 3.3 — PATCH with a missing field leaves that field unchanged.
#[tokio::test]
async fn patch_settings_missing_field_leaves_unchanged() {
    let (server, db) = logged_in_server_with_db().await;
    let user_id = get_user_id(&server).await;
    insert_project(&db, "proj-partial", &user_id).await;

    // Only update auto_stop_timeout_mins; the other two fields should stay at defaults.
    let resp = server
        .patch("/projects/proj-partial/settings")
        .json(&json!({"auto_stop_timeout_mins": 30}))
        .await;
    resp.assert_status(StatusCode::OK);
    let body: Value = resp.json();

    assert_eq!(body["auto_stop_timeout_mins"], json!(30));
    // Unchanged defaults
    assert_eq!(body["auto_stop_enabled"], json!(true));
    assert_eq!(body["auto_start_enabled"], json!(true));
}

/// Requirements 3.1 — PATCH on a non-existent project returns 404.
#[tokio::test]
async fn patch_settings_nonexistent_project_returns_404() {
    let server = logged_in_server().await;
    server
        .patch("/projects/does-not-exist/settings")
        .json(&json!({"auto_stop_enabled": false}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// Requirements 3.2 — PATCH with auto_stop_timeout_mins=0 returns 400.
#[tokio::test]
async fn patch_settings_zero_timeout_returns_400() {
    let (server, db) = logged_in_server_with_db().await;
    let user_id = get_user_id(&server).await;
    insert_project(&db, "proj-zero", &user_id).await;

    server
        .patch("/projects/proj-zero/settings")
        .json(&json!({"auto_stop_timeout_mins": 0}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // Stored value must remain unchanged (still 15).
    let resp = server.get("/projects/proj-zero").await;
    let body: Value = resp.json();
    assert_eq!(body["auto_stop_timeout_mins"], json!(15));
}

#[cfg(test)]
mod prop_tests {
    use proptest::prelude::*;
    use serde_json::{json, Value};
    use axum::http::StatusCode;

    use super::{logged_in_server_with_db, insert_project, get_user_id};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]
        #[test]
        fn prop_settings_round_trip(
            auto_stop_enabled: bool,
            auto_stop_timeout_mins in 1i64..=10080i64,
            auto_start_enabled: bool,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (server, db) = logged_in_server_with_db().await;
                let user_id = get_user_id(&server).await;
                insert_project(&db, "prop-rt-proj", &user_id).await;

                // PATCH the settings
                let patch_resp = server
                    .patch("/projects/prop-rt-proj/settings")
                    .json(&json!({
                        "auto_stop_enabled": auto_stop_enabled,
                        "auto_stop_timeout_mins": auto_stop_timeout_mins,
                        "auto_start_enabled": auto_start_enabled,
                    }))
                    .await;
                patch_resp.assert_status(StatusCode::OK);

                // GET the project and verify values match
                let get_resp = server.get("/projects/prop-rt-proj").await;
                get_resp.assert_status(StatusCode::OK);
                let body: Value = get_resp.json();

                prop_assert_eq!(&body["auto_stop_enabled"], &json!(auto_stop_enabled));
                prop_assert_eq!(&body["auto_stop_timeout_mins"], &json!(auto_stop_timeout_mins));
                prop_assert_eq!(&body["auto_start_enabled"], &json!(auto_start_enabled));

                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]
        #[test]
        fn prop_invalid_timeout_rejected(
            timeout in (-10000i64)..=0i64,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (server, db) = logged_in_server_with_db().await;
                let user_id = get_user_id(&server).await;
                insert_project(&db, "prop-inv-proj", &user_id).await;

                // PATCH with an invalid timeout — must return 400
                let patch_resp = server
                    .patch("/projects/prop-inv-proj/settings")
                    .json(&json!({"auto_stop_timeout_mins": timeout}))
                    .await;
                prop_assert_eq!(patch_resp.status_code(), StatusCode::BAD_REQUEST);

                // GET the project and verify the stored value is still the default (15)
                let get_resp = server.get("/projects/prop-inv-proj").await;
                get_resp.assert_status(StatusCode::OK);
                let body: Value = get_resp.json();
                prop_assert_eq!(&body["auto_stop_timeout_mins"], &json!(15));

                Ok(())
            }).unwrap();
        }
    }
}
