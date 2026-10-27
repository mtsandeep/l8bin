use axum::http::StatusCode;
use axum_test::TestServer;
use sqlx::SqlitePool;

use crate::tests::helpers::test_server_with_db;

// Env-var-backed folders are process-global; tests that repoint them serialize.
static EXPORTS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static IMPORTS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

async fn insert_project(db: &SqlitePool, id: &str, name: Option<&str>, status: &str, deploy_type: Option<&str>) {
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(db)
        .await
        .expect("login first so a user row exists");
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects (id, user_id, name, status, node_id, deploy_type, is_background, \
         auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, allow_raw_ports, \
         allow_docker_access, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'local', ?, 0, 1, 15, 0, 0, 0, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(status)
    .bind(deploy_type)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .unwrap();
}

async fn login(server: &TestServer) {
    // Register (first user becomes admin) + login, so manage routes authorize.
    server
        .post("/auth/register")
        .json(&serde_json::json!({"username": "tester", "password": "test-password-123"}))
        .await;
    server.post("/auth/login").json(&serde_json::json!({"username": "tester", "password": "test-password-123"})).await;
}

#[tokio::test]
async fn duplicate_unknown_project_returns_404() {
    let (server, _db) = test_server_with_db().await;
    login(&server).await;
    let res = server.post("/projects/nope/duplicate").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn duplicate_conflicts_while_transient() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "busy", Some("Busy"), "deploying", Some("compose")).await;
    let res = server.post("/projects/busy/duplicate").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn duplicate_creates_rows_with_suffixed_id() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "my-app", Some("My App"), "running", Some("compose")).await;
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO project_services (project_id, service_name, image, port, is_public, status) \
         VALUES ('my-app', 'db', 'postgres:16', 5432, 0, 'running')",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_volumes (project_id, service_name, volume_name, container_path) \
         VALUES ('my-app', 'db', 'litebin_my-app_pgdata', '/var/lib/postgresql/data')",
    )
    .execute(&db)
    .await
    .unwrap();
    for (id, route_type, path, sub) in [
        ("alias-row", "alias", None::<String>, Some("short".to_string())),
        ("path-row", "path", Some("/api".to_string()), None),
    ] {
        sqlx::query(
            "INSERT INTO project_routes (id, project_id, route_type, path, subdomain, upstream, priority, created_at) \
             VALUES (?, 'my-app', ?, ?, ?, 'db:5432', 100, 0)",
        )
        .bind(id)
        .bind(route_type)
        .bind(path)
        .bind(sub)
        .execute(&db)
        .await
        .unwrap();
    }
    let _ = now;

    // Colliding requested id → auto-suffix, warning, rows created.
    let res = server
        .post("/projects/my-app/duplicate")
        .json(&serde_json::json!({ "new_id": "my-app", "name": "Staging copy", "target_node_id": "local" }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert_eq!(body["project_id"], "my-app-2");
    assert!(body["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("'my-app' was taken")));

    let copied: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT name, node_id FROM projects WHERE id = 'my-app-2'").fetch_optional(&db).await.unwrap();
    let (name, node_id) = copied.expect("copy row created");
    assert_eq!(name.as_deref(), Some("Staging copy"));
    assert_eq!(node_id.as_deref(), None, "local target stores NULL node_id");

    let service_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_services WHERE project_id = 'my-app-2'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(service_count, 1);

    let volume: Option<String> =
        sqlx::query_scalar("SELECT volume_name FROM project_volumes WHERE project_id = 'my-app-2'")
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(volume.as_deref(), Some("litebin_my-app-2_pgdata"), "volume re-scoped to the new id");

    // The source keeps its alias — the copy must not double-claim the subdomain;
    // the path route still copies.
    let routes: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT route_type, subdomain FROM project_routes WHERE project_id = 'my-app-2'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].0, "path");
    assert!(
        body["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("alias 'short' is taken"))
    );
}

#[tokio::test]
async fn export_unknown_project_returns_404() {
    let (server, _db) = test_server_with_db().await;
    login(&server).await;
    let res = server.post("/projects/nope/export").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn export_conflicts_while_transient() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "busy", Some("Busy"), "migrating", Some("compose")).await;
    let res = server.post("/projects/busy/export").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn export_starts_job_and_reports_status() {
    let _guard = EXPORTS_DIR_LOCK.lock().unwrap();
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    // The job may reach the real local Docker daemon and actually write a
    // bundle — point the exports folder at a temp dir so nothing lands in the
    // repo's data dir. Assertions stick to the deterministic parts (job
    // registration + status flip).
    let dir = std::env::temp_dir().join(format!("l8b-export-job-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: EXPORTS_DIR is process-global; this test holds EXPORTS_DIR_LOCK.
    unsafe {
        std::env::set_var("EXPORTS_DIR", &dir);
    }
    insert_project(&db, "idle", Some("Idle"), "stopped", Some("compose")).await;
    let res = server.post("/projects/idle/export").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert_eq!(body["status"], "exporting");
    let job_id = body["job_id"].as_str().expect("job id").to_string();

    let job = server.get(&format!("/exports/jobs/{job_id}")).await;
    assert_eq!(job.status_code(), StatusCode::OK);
    let j: serde_json::Value = job.json();
    assert_eq!(j["project_id"], "idle");
    assert!(matches!(j["status"].as_str(), Some("running") | Some("completed") | Some("failed")));

    let status: String =
        sqlx::query_scalar("SELECT status FROM projects WHERE id = 'idle'").fetch_one(&db).await.unwrap();
    assert_eq!(status, "exporting");

    // SAFETY: see above.
    unsafe {
        std::env::remove_var("EXPORTS_DIR");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn exports_folder_list_and_delete() {
    let _guard = EXPORTS_DIR_LOCK.lock().unwrap();
    let (server, _db) = test_server_with_db().await;
    login(&server).await;

    // Point the exports folder at a temp dir with one bundle in it.
    let dir = std::env::temp_dir().join(format!("l8b-exports-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("demo-20260101-000000.l8b.tar.gz"), b"fake").unwrap();
    // SAFETY: tests touching EXPORTS_DIR run serially in practice; restore after.
    unsafe {
        std::env::set_var("EXPORTS_DIR", &dir);
    }

    let list: serde_json::Value = server.get("/exports").await.json();
    let files: Vec<String> =
        list["exports"].as_array().unwrap().iter().map(|e| e["file"].as_str().unwrap().to_string()).collect();
    assert!(files.contains(&"demo-20260101-000000.l8b.tar.gz".to_string()));

    // Traversal names are rejected outright (no file bytes served).
    let res = server.get("/exports/..%2F..%2Fetc%2Fpasswd").await;
    assert!(!res.status_code().is_success());

    let res = server.delete("/exports/demo-20260101-000000.l8b.tar.gz").await;
    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
    let res = server.delete("/exports/demo-20260101-000000.l8b.tar.gz").await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);

    // SAFETY: see above.
    unsafe {
        std::env::remove_var("EXPORTS_DIR");
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ── Import ───────────────────────────────────────────────────────────────────

fn full_manifest_json(id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "format": "litebin.export",
        "version": 1,
        "created_at": 1,
        "source": { "master_version": "t", "node_id": "local", "architecture": "x86_64" },
        "project": {
            "id": id, "name": "Bundled App", "description": "from another master",
            "is_background": false, "image": null, "internal_port": 3000, "cmd": null,
            "memory_limit_mb": null, "cpu_limit": null, "auto_stop_enabled": true,
            "auto_stop_timeout_mins": 15, "auto_start_enabled": false,
            "allow_raw_ports": false, "allow_docker_access": false,
            "volumes": null, "deploy_type": "compose", "custom_domain": "app.example.com"
        },
        "services": [
            { "service_name": "web", "image": "nginx:1.27", "port": 80, "cmd": null,
              "is_public": true, "depends_on": null, "memory_limit_mb": null,
              "cpu_limit": null, "is_oneshot": false }
        ],
        "volumes": [
            { "scoped_name": format!("litebin_{id}_data"), "service_name": "web",
              "container_path": "/data" }
        ],
        "images": [],
        "routes": [
            { "route_type": "path", "path": "/api", "subdomain": null, "upstream": "web:3000", "priority": 100 },
            { "route_type": "alias", "path": null, "subdomain": "short", "upstream": "web:3000", "priority": 100 }
        ],
        "flags": { "include_env": true, "include_volumes": true }
    }))
    .unwrap()
}

fn project_dir_tar(id: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in [
        (format!("{id}/compose.yaml"), "services: {}".as_bytes().to_vec()),
        (format!("{id}/.env"), b"SECRET=1".to_vec()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, path.as_str(), data.as_slice()).unwrap();
    }
    builder.into_inner().unwrap()
}

/// Build a valid bundle in `dir` named `file`, with manifest for `id`.
async fn write_test_bundle(dir: &std::path::Path, file: &str, id: &str) {
    let tmp = dir.join("staging");
    std::fs::create_dir_all(&tmp).unwrap();
    let project_spool = tmp.join("project.tar");
    std::fs::write(&project_spool, project_dir_tar(id)).unwrap();
    let volume_spool = tmp.join("volume.tar");
    std::fs::write(&volume_spool, b"volume-contents").unwrap();

    let mut pending =
        vec![("project.tar".to_string(), project_spool), (format!("volumes/litebin_{id}_data.tar"), volume_spool)];
    pending.reverse();
    crate::copy_engine::bundle::write_bundle(&dir.join(file), full_manifest_json(id), move || {
        let next = pending.pop();
        async move { Ok(next) }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn import_from_folder_creates_project_with_new_id() {
    let _guard = IMPORTS_DIR_LOCK.lock().unwrap();
    let (server, db) = test_server_with_db().await;
    login(&server).await;

    let dir = std::env::temp_dir().join(format!("l8b-imports-test-{}", uuid::Uuid::new_v4()));
    write_test_bundle(&dir, "bundled.l8b.tar.gz", "bundle-app").await;
    // SAFETY: IMPORTS_DIR is process-global; this test holds IMPORTS_DIR_LOCK.
    unsafe {
        std::env::set_var("IMPORTS_DIR", &dir);
    }

    // Pre-existing project with the same id → auto-suffix.
    insert_project(&db, "bundle-app", Some("Existing"), "running", Some("compose")).await;
    let res = server
        .post("/imports")
        .json(&serde_json::json!({ "file": "bundled.l8b.tar.gz", "target_node_id": "local" }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert_eq!(body["project_id"], "bundle-app-2");

    let row: Option<(String, String)> = sqlx::query_as("SELECT name, status FROM projects WHERE id = 'bundle-app-2'")
        .fetch_optional(&db)
        .await
        .unwrap();
    let (name, status) = row.expect("imported project row");
    assert_eq!(name, "Bundled App");
    assert!(status == "importing" || status == "stopped" || status == "error", "got {status}");

    let volume: Option<String> =
        sqlx::query_scalar("SELECT volume_name FROM project_volumes WHERE project_id = 'bundle-app-2'")
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(volume.as_deref(), Some("litebin_bundle-app-2_data"), "volume re-scoped on import");

    let service: Option<String> =
        sqlx::query_scalar("SELECT image FROM project_services WHERE project_id = 'bundle-app-2'")
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(service.as_deref(), Some("nginx:1.27"));

    // Custom domain + routes travel with the bundle.
    let domain: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT custom_domain FROM projects WHERE id = 'bundle-app-2'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(domain.as_deref(), Some("app.example.com"));

    let route_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_routes WHERE project_id = 'bundle-app-2'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(route_count, 2, "path route + alias carried over");

    // Folder listing sees the bundle.
    let list: serde_json::Value = server.get("/imports").await.json();
    let files: Vec<String> =
        list["archives"].as_array().unwrap().iter().map(|a| a["file"].as_str().unwrap().to_string()).collect();
    assert!(files.contains(&"bundled.l8b.tar.gz".to_string()));

    // SAFETY: see above.
    unsafe {
        std::env::remove_var("IMPORTS_DIR");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn import_skips_claimed_custom_domain_and_aliases() {
    let _guard = IMPORTS_DIR_LOCK.lock().unwrap();
    let (server, db) = test_server_with_db().await;
    login(&server).await;

    let dir = std::env::temp_dir().join(format!("l8b-imports-claims-{}", uuid::Uuid::new_v4()));
    write_test_bundle(&dir, "claimed.l8b.tar.gz", "claim-app").await;
    // SAFETY: IMPORTS_DIR is process-global; this test holds IMPORTS_DIR_LOCK.
    unsafe {
        std::env::set_var("IMPORTS_DIR", &dir);
    }

    // Another project already holds the custom domain and the alias subdomain.
    insert_project(&db, "holder", Some("Holder"), "running", Some("compose")).await;
    sqlx::query("UPDATE projects SET custom_domain = 'app.example.com' WHERE id = 'holder'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO project_routes (id, project_id, route_type, path, subdomain, upstream, priority, created_at) \
         VALUES ('r1', 'holder', 'alias', NULL, 'short', 'web:3000', 100, 0)",
    )
    .execute(&db)
    .await
    .unwrap();

    let res = server
        .post("/imports")
        .json(&serde_json::json!({ "file": "claimed.l8b.tar.gz", "target_node_id": "local", "start_after_import": false }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    let warnings: Vec<String> =
        body["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect();
    assert!(warnings.iter().any(|w| w.contains("custom domain 'app.example.com'")));
    assert!(warnings.iter().any(|w| w.contains("alias 'short'")));

    // The imported project carries neither claim; the path route still copied.
    let domain: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT custom_domain FROM projects WHERE id = 'claim-app'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(domain, None);
    let routes: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT route_type, subdomain FROM project_routes WHERE project_id = 'claim-app'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].0, "path");

    // SAFETY: see above.
    unsafe {
        std::env::remove_var("IMPORTS_DIR");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn import_rejects_bad_bundles_and_names() {
    let _guard = IMPORTS_DIR_LOCK.lock().unwrap();
    let (server, _db) = test_server_with_db().await;
    login(&server).await;

    let dir = std::env::temp_dir().join(format!("l8b-imports-bad-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: IMPORTS_DIR is process-global; this test holds IMPORTS_DIR_LOCK.
    unsafe {
        std::env::set_var("IMPORTS_DIR", &dir);
    }

    // Traversal-style names are rejected before any file access.
    let res = server.post("/imports").json(&serde_json::json!({ "file": "../x" })).await;
    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);

    // A tar without a manifest is not a litebin bundle.
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(2);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "random.txt", &b"hi"[..]).unwrap();
    std::fs::write(dir.join("random.tar"), builder.into_inner().unwrap()).unwrap();
    let res = server.post("/imports").json(&serde_json::json!({ "file": "random.tar" })).await;
    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);

    // Unknown file → 404.
    let res = server.post("/imports").json(&serde_json::json!({ "file": "missing.l8b.tar.gz" })).await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);

    // SAFETY: see above.
    unsafe {
        std::env::remove_var("IMPORTS_DIR");
    }
    std::fs::remove_dir_all(&dir).ok();
}
