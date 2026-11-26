use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::{TestServer, TestServerConfig};
use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use crate::tests::helpers::{build_router, build_test_state, test_config, test_server_with_db};

// Env-var-backed folders are process-global; tests that repoint them serialize.
static EXPORTS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static IMPORTS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Points `EXPORTS_DIR` at a fresh temp dir for the duration of a test, to the
/// exclusion of the other tests that repoint it. A poisoned lock is taken over
/// — it only excludes, it guards no invariant — and the env var is restored on
/// drop, so a failed assertion can neither fail the next test nor leak the
/// folder into it.
struct ExportsDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: std::path::PathBuf,
}

impl ExportsDir {
    fn new(tag: &str) -> Self {
        let lock = EXPORTS_DIR_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: EXPORTS_DIR is process-global; this test holds EXPORTS_DIR_LOCK.
        unsafe {
            std::env::set_var("EXPORTS_DIR", &dir);
        }
        Self { _lock: lock, dir }
    }
}

impl Drop for ExportsDir {
    fn drop(&mut self) {
        // SAFETY: EXPORTS_DIR is process-global; this test holds EXPORTS_DIR_LOCK.
        unsafe {
            std::env::remove_var("EXPORTS_DIR");
        }
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

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
async fn duplicate_carries_raw_ports_with_conflict_warning() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "ports-app", Some("Ports"), "stopped", Some("compose")).await;
    sqlx::query("UPDATE projects SET allow_raw_ports = 1 WHERE id = 'ports-app'").execute(&db).await.unwrap();

    let res = server
        .post("/projects/ports-app/duplicate")
        .json(&serde_json::json!({ "target_node_id": "local", "start_after": false }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert!(body["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("raw ports enabled")));

    let flag: bool = sqlx::query_scalar("SELECT allow_raw_ports FROM projects WHERE id = 'ports-app-copy'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(flag, "copy carries the raw ports permission");
}

#[tokio::test]
async fn duplicate_warns_about_anonymous_volume_mounts() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "anon-app", Some("Anon"), "running", Some("compose")).await;
    sqlx::query(
        "INSERT INTO project_volumes (project_id, service_name, volume_name, container_path) \
         VALUES ('anon-app', 'db', NULL, '/var/lib/postgresql/data')",
    )
    .execute(&db)
    .await
    .unwrap();

    let res = server
        .post("/projects/anon-app/duplicate")
        .json(&serde_json::json!({ "target_node_id": "local", "start_after": false }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert!(
        body["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("anonymous volume mounts"))
    );
}

#[tokio::test]
async fn duplicate_rejects_cross_arch_uploaded_images() {
    let (server, db) = test_server_with_db().await;
    login(&server).await;
    insert_project(&db, "static", Some("Static"), "running", Some("image")).await;
    sqlx::query("UPDATE projects SET image = 'sha256:abcdef' WHERE id = 'static'").execute(&db).await.unwrap();

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO nodes (id, name, host, agent_port, status, architecture, fail_count, created_at, updated_at) \
         VALUES ('arm-node', 'ARM', '10.0.0.2', 5083, 'online', 'aarch64', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&db)
    .await
    .unwrap();
    // The local node row reports x86_64 so the architectures differ.
    sqlx::query("UPDATE nodes SET architecture = 'x86_64' WHERE id = 'local'").execute(&db).await.unwrap();

    let res =
        server.post("/projects/static/duplicate").json(&serde_json::json!({ "target_node_id": "arm-node" })).await;
    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json();
    assert!(body["error"].as_str().unwrap().contains("uploaded (sha256:) images"));

    // Registry-ref projects still duplicate cross-arch, with a warning.
    sqlx::query("UPDATE projects SET image = 'nginx:1.27' WHERE id = 'static'").execute(&db).await.unwrap();
    let res = server
        .post("/projects/static/duplicate")
        .json(&serde_json::json!({ "target_node_id": "arm-node", "start_after": false }))
        .await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert!(body["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("architectures differ")));
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
    let _exports = ExportsDir::new("l8b-export-job");
    let (state, db) = build_test_state(test_config()).await;
    let server = TestServer::new_with_config(
        build_router(state.clone()),
        TestServerConfig { save_cookies: true, ..TestServerConfig::new() },
    )
    .unwrap();
    login(&server).await;
    insert_project(&db, "idle", Some("Idle"), "stopped", Some("compose")).await;

    // The job's first step stops the source under the project's operation lock.
    // Holding that lock keeps the job in flight, so "exporting" is observed
    // rather than raced: the project goes back to "stopped" once the job ends,
    // and whether that beats the reads below is up to the scheduler and to how
    // fast the local Docker daemon answers.
    let operation_lock =
        state.project_locks.entry("idle".to_string()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let hold = operation_lock.acquire_owned().await.unwrap();

    let res = server.post("/projects/idle/export").json(&serde_json::json!({})).await;
    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: serde_json::Value = res.json();
    assert_eq!(body["status"], "exporting");
    let job_id = body["job_id"].as_str().expect("job id").to_string();

    let job = server.get(&format!("/exports/jobs/{job_id}")).await;
    assert_eq!(job.status_code(), StatusCode::OK);
    let j: serde_json::Value = job.json();
    assert_eq!(j["project_id"], "idle");
    assert_eq!(j["status"], "running");
    let status: String =
        sqlx::query_scalar("SELECT status FROM projects WHERE id = 'idle'").fetch_one(&db).await.unwrap();
    assert_eq!(status, "exporting");

    // Released, the job runs to its end. It may reach the real local Docker
    // daemon and write a bundle, or fail without one — either way it settles
    // and the project returns to the status it had before the export. A failed
    // job is reported just before its project status is restored, so wait for
    // both rather than read the project the moment the job settles.
    drop(hold);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let j = loop {
        let j: serde_json::Value = server.get(&format!("/exports/jobs/{job_id}")).await.json();
        let status: String =
            sqlx::query_scalar("SELECT status FROM projects WHERE id = 'idle'").fetch_one(&db).await.unwrap();
        if j["status"] != "running" && status == "stopped" {
            break j;
        }
        // Until then the export is the only thing that may have moved the project.
        assert!(status == "exporting", "project is '{status}' while its export job is {}", j["status"]);
        assert!(
            tokio::time::Instant::now() < deadline,
            "export job '{job_id}' did not settle: job {}, project '{status}'",
            j["status"]
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(matches!(j["status"].as_str(), Some("completed") | Some("failed")), "{j}");
}

#[tokio::test]
async fn exports_folder_list_and_delete() {
    let exports = ExportsDir::new("l8b-exports-test");
    let (server, _db) = test_server_with_db().await;
    login(&server).await;

    // One bundle in the exports folder.
    std::fs::write(exports.dir.join("demo-20260101-000000.l8b.tar.gz"), b"fake").unwrap();

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

    let image_spool = tmp.join("image.tar");
    std::fs::write(&image_spool, b"docker-save-tar").unwrap();
    let mut pending = vec![
        ("project.tar".to_string(), project_spool),
        (format!("volumes/litebin_{id}_data.tar"), volume_spool),
        ("images/sha256-d7b91807d0bbc094e6393d2063eaf9eac28875565573fe1ec73b14a76f60c26f.tar".to_string(), image_spool),
    ];
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
