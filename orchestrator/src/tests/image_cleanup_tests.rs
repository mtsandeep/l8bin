//! Image-cleanup regression tests: deleting a project must not remove an image
//! digest that another project still references.
//!
//! Docker calls fail fast against the `new_for_tests` dead endpoint and are
//! swallowed by the best-effort cleanup path, so assertions run against the DB
//! reference state.

use axum::http::StatusCode;
use axum_test::TestServer;
use sqlx::SqlitePool;

use crate::routes::manage::helpers::image_referenced_by_other_projects;
use crate::tests::helpers::test_server_with_db;

async fn insert_project(db: &SqlitePool, id: &str, status: &str, deploy_type: Option<&str>) {
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(db)
        .await
        .expect("login first so a user row exists");
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects (id, user_id, status, node_id, deploy_type, is_background, \
         auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports, \
         allow_docker_access, created_at, updated_at) \
         VALUES (?, ?, ?, 'local', ?, 0, 1, 15, 0, 0, 0, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(status)
    .bind(deploy_type)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

async fn insert_service(db: &SqlitePool, project_id: &str, service: &str, image: &str) {
    sqlx::query("INSERT INTO project_services (project_id, service_name, image) VALUES (?, ?, ?)")
        .bind(project_id)
        .bind(service)
        .bind(image)
        .execute(db)
        .await
        .unwrap();
}

async fn login(server: &TestServer) {
    server
        .post("/auth/register")
        .json(&serde_json::json!({"username": "tester", "password": "test-password-123"}))
        .await;
    server.post("/auth/login").json(&serde_json::json!({"username": "tester", "password": "test-password-123"})).await;
}

/// Demo-child + parent sharing one digest:
/// deleting the child must leave the parent's references intact and the digest
/// must still count as referenced; deleting the parent must release it.
#[tokio::test]
async fn deleting_demo_child_keeps_parent_digest_referenced() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;

    // Sleeping parent with wake-time services; child rows are verbatim copies.
    insert_project(&db, "myapp", "stopped", Some("compose")).await;
    insert_service(&db, "myapp", "bootstrap", "sha256:c2524aeb").await;
    insert_service(&db, "myapp", "ui", "sha256:c2524aeb").await;
    insert_project(&db, "myapp-edit-ad6b", "stopped", Some("compose")).await;
    insert_service(&db, "myapp-edit-ad6b", "bootstrap", "sha256:c2524aeb").await;

    let res = server.delete("/projects/myapp-edit-ad6b").await;
    assert_eq!(res.status_code(), StatusCode::OK);

    // Child rows are gone...
    let child_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_services WHERE project_id = 'myapp-edit-ad6b'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(child_rows, 0, "child service rows must cascade away");

    // ...but the digest must still be referenced by the parent.
    assert!(
        image_referenced_by_other_projects(&db, "local", "sha256:c2524aeb").await.unwrap(),
        "digest referenced by the surviving parent must still count as referenced"
    );
    let parent_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_services WHERE project_id = 'myapp' AND image = 'sha256:c2524aeb'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(parent_rows, 2, "parent service rows must be untouched");
}

/// Deleting the last project referencing a digest releases it for cleanup.
#[tokio::test]
async fn deleting_last_referencing_project_releases_digest() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;

    insert_project(&db, "solo", "stopped", Some("compose")).await;
    insert_service(&db, "solo", "web", "sha256:lonely").await;
    assert!(image_referenced_by_other_projects(&db, "local", "sha256:lonely").await.unwrap());

    let res = server.delete("/projects/solo").await;
    assert_eq!(res.status_code(), StatusCode::OK);

    assert!(
        !image_referenced_by_other_projects(&db, "local", "sha256:lonely").await.unwrap(),
        "once the last referencing project is deleted, nothing blocks removal"
    );
}

/// Duplicates share digests: deleting the source must not release the digest
/// while the duplicate still references it.
#[tokio::test]
async fn duplicate_keeps_digest_referenced_after_source_delete() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;

    // Same-node duplicate: source and copy both reference the digest.
    insert_project(&db, "app", "stopped", Some("compose")).await;
    insert_service(&db, "app", "web", "sha256:dup").await;
    insert_project(&db, "app-copy", "stopped", Some("compose")).await;
    insert_service(&db, "app-copy", "web", "sha256:dup").await;

    let res = server.delete("/projects/app").await;
    assert_eq!(res.status_code(), StatusCode::OK);

    assert!(
        image_referenced_by_other_projects(&db, "local", "sha256:dup").await.unwrap(),
        "duplicate still references the digest after source deletion"
    );

    let res = server.delete("/projects/app-copy").await;
    assert_eq!(res.status_code(), StatusCode::OK);
    assert!(!image_referenced_by_other_projects(&db, "local", "sha256:dup").await.unwrap());
}
