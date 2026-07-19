use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use dashmap::DashMap;
use litebin_common::docker::DockerManager;
use litebin_common::routing::{MasterProxyRouter, RoutingProvider};
use litebin_common::types::{DeployType, Project, ProjectStatus};
use sqlx::SqlitePool;
use tokio::sync::RwLock as AsyncRwLock;

use crate::AppState;
use crate::routes::manage::handlers::{
    RecreateRequest, delete_project, recreate_project, start_project, stop_project,
};
use crate::routes::manage::multi_service::{recreate_services, stop_services};
use crate::routes::manage::{StartServicesOpts, start_services};
use crate::routes::stats::{LogsQuery, project_logs};

const SERVICE_NAME: &str = "worker";
const LOG_MARKER: &str = "orchestrator-path-log";

fn unique_project_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn native_test_image() -> anyhow::Result<String> {
    let image = std::env::var("LITEBIN_NATIVE_TEST_IMAGE").map_err(|_| {
        anyhow::anyhow!(
            "LITEBIN_NATIVE_TEST_IMAGE must name a locally available image with /bin/sh and sleep"
        )
    })?;
    anyhow::ensure!(
        !image.contains(['\r', '\n', '"']),
        "LITEBIN_NATIVE_TEST_IMAGE contains unsupported characters"
    );
    Ok(image)
}

fn compose_yaml(image: &str) -> String {
    format!(
        r#"services:
  {SERVICE_NAME}:
    image: "{image}"
    command: ["/bin/sh", "-c", "echo {LOG_MARKER}; exec sleep 300"]
"#
    )
}

async fn live_orchestrator_state() -> anyhow::Result<AppState> {
    let db = SqlitePool::connect("sqlite::memory:").await?;
    sqlx::migrate!("src/db/migrations").run(&db).await?;
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO nodes
         (id, name, host, agent_port, status, fail_count, created_at, updated_at)
         VALUES ('local', 'Local', 'localhost', 0, 'online', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&db)
    .await?;

    let mut docker = DockerManager::new(
        "litebin-orchestrator-path-tests".into(),
        128 * 1024 * 1024,
        0.25,
    )?;
    docker.detect_host_projects_dir().await;
    let router: Arc<AsyncRwLock<Arc<dyn RoutingProvider>>> =
        Arc::new(AsyncRwLock::new(Arc::new(MasterProxyRouter::new(
            litebin_common::caddy::CaddyClient::new("http://127.0.0.1:1"),
            String::new(),
        ))));
    let (route_sync_tx, _) = tokio::sync::mpsc::unbounded_channel();

    Ok(AppState {
        config: Arc::new(super::helpers::test_config()),
        db,
        docker: Arc::new(docker),
        router,
        node_clients: Arc::new(DashMap::new()),
        disk_cache: Arc::new(DashMap::new()),
        project_locks: Arc::new(DashMap::new()),
        wake_failures: Arc::new(DashMap::new()),
        route_sync_tx,
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
        deploy_logs: Arc::new(DashMap::new()),
    })
}

async fn insert_compose_project(
    db: &SqlitePool,
    project_id: &str,
    node_id: &str,
    image: &str,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO projects
         (id, user_id, is_background, status, node_id, auto_stop_enabled,
          auto_start_enabled, service_count, service_summary, deploy_type,
          created_at, updated_at)
         VALUES (?, 'system', 1, 'stopped', ?, 0, 0, 1, ?, 'compose', ?, ?)",
    )
    .bind(project_id)
    .bind(node_id)
    .bind(SERVICE_NAME)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    sqlx::query(
        "INSERT INTO project_services
         (project_id, service_name, image, is_public, status)
         VALUES (?, ?, ?, 0, 'stopped')",
    )
    .bind(project_id)
    .bind(SERVICE_NAME)
    .bind(image)
    .execute(db)
    .await?;

    // Keep the prerequisite image referenced so production delete cleanup does
    // not remove a caller-provided local image after the test.
    let keeper_id = format!("{project_id}-image-keeper");
    sqlx::query(
        "INSERT INTO projects
         (id, user_id, status, node_id, deploy_type, created_at, updated_at)
         VALUES (?, 'system', 'stopped', ?, 'compose', ?, ?)",
    )
    .bind(&keeper_id)
    .bind(node_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    sqlx::query(
        "INSERT INTO project_services
         (project_id, service_name, image, is_public, status)
         VALUES (?, 'image-keeper', ?, 0, 'stopped')",
    )
    .bind(keeper_id)
    .bind(image)
    .execute(db)
    .await?;
    Ok(())
}

fn write_compose(project_id: &str, image: &str) -> anyhow::Result<()> {
    let dir = PathBuf::from("projects").join(project_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("compose.yaml"), compose_yaml(image))?;
    Ok(())
}

async fn project(db: &SqlitePool, project_id: &str) -> anyhow::Result<Project> {
    Ok(sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await?)
}

async fn service_row(
    db: &SqlitePool,
    project_id: &str,
) -> anyhow::Result<(Option<String>, String, bool)> {
    Ok(sqlx::query_as(
        "SELECT container_id, status, is_public
         FROM project_services WHERE project_id = ? AND service_name = ?",
    )
    .bind(project_id)
    .bind(SERVICE_NAME)
    .fetch_one(db)
    .await?)
}

async fn cleanup_live_project(state: &AppState, project_id: &str) {
    let _ = state
        .docker
        .cleanup_project_resources(project_id, &[])
        .await;
    let _ = std::fs::remove_dir_all(PathBuf::from("projects").join(project_id));
}

async fn assert_background_compose_state(
    state: &AppState,
    project_id: &str,
) -> anyhow::Result<String> {
    let project = project(&state.db, project_id).await?;
    anyhow::ensure!(project.deploy_type == Some(DeployType::Compose));
    anyhow::ensure!(project.is_background);
    anyhow::ensure!(project.status == ProjectStatus::Running);
    anyhow::ensure!(project.container_id.is_none());
    anyhow::ensure!(project.mapped_port.is_none());
    let response =
        crate::routes::projects::get_project(State(state.clone()), Path(project_id.to_string()))
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?
            .0;
    anyhow::ensure!(response.deploy_type.as_deref() == Some("compose"));
    anyhow::ensure!(response.public_stats.is_none());
    let (container_id, status, is_public) = service_row(&state.db, project_id).await?;
    anyhow::ensure!(status == "running");
    anyhow::ensure!(!is_public);
    container_id.ok_or_else(|| anyhow::anyhow!("service row has no container id"))
}

#[tokio::test]
#[ignore = "requires native Docker, permission to create containers and networks, and LITEBIN_NATIVE_TEST_IMAGE preloaded with /bin/sh and sleep"]
async fn live_local_compose_background_uses_orchestrator_lifecycle() {
    let image = native_test_image().unwrap();
    let project_id = unique_project_id("local-compose-path");
    let state = live_orchestrator_state().await.unwrap();
    cleanup_live_project(&state, &project_id).await;

    let result: anyhow::Result<()> = async {
        insert_compose_project(&state.db, &project_id, "local", &image).await?;
        write_compose(&project_id, &image)?;
        let initial = project(&state.db, &project_id).await?;

        start_services(
            &state,
            &initial,
            StartServicesOpts {
                pull_images: false,
                connect_orchestrator: false,
                rollback_on_failure: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let first_id = assert_background_compose_state(&state, &project_id).await?;

        let logs = project_logs(
            State(state.clone()),
            Path(project_id.clone()),
            Query(LogsQuery {
                tail: Some(50),
                service: Some(SERVICE_NAME.into()),
            }),
        )
        .await
        .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?
        .0;
        anyhow::ensure!(logs.service_name.as_deref() == Some(SERVICE_NAME));
        anyhow::ensure!(logs.lines.iter().any(|line| line.contains(LOG_MARKER)));

        stop_services(&state, &project_id, None)
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let (_, stopped_status, _) = service_row(&state.db, &project_id).await?;
        anyhow::ensure!(stopped_status == "stopped");

        let stopped = project(&state.db, &project_id).await?;
        let _ = recreate_services(&state, &stopped, None, false)
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let recreated_id = assert_background_compose_state(&state, &project_id).await?;
        anyhow::ensure!(recreated_id != first_id);

        let _ = delete_project(State(state.clone()), Path(project_id.clone()))
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_one(&state.db)
            .await?;
        anyhow::ensure!(remaining == 0);
        anyhow::ensure!(
            state
                .docker
                .list_project_workload_containers(&project_id)
                .await?
                .is_empty()
        );
        Ok(())
    }
    .await;

    cleanup_live_project(&state, &project_id).await;
    result.unwrap();
}

async fn live_agent_state() -> anyhow::Result<litebin_agent::AgentState> {
    let mut docker = DockerManager::new(
        "litebin-orchestrator-path-tests".into(),
        128 * 1024 * 1024,
        0.25,
    )?;
    docker.detect_host_projects_dir().await;
    Ok(litebin_agent::AgentState {
        config: Arc::new(litebin_agent::Config {
            agent_port: 0,
            cert_path: String::new(),
            key_path: String::new(),
            ca_cert_path: String::new(),
            public_ip: String::new(),
            caddy_admin_url: "http://127.0.0.1:1".into(),
            cert_pem: String::new(),
            key_pem: String::new(),
        }),
        docker: Arc::new(docker),
        caddy: None,
        wake_locks: Arc::new(DashMap::new()),
        registration: Arc::new(RwLock::new(None)),
        last_caddy_config: Arc::new(RwLock::new(None)),
        project_meta: Arc::new(RwLock::new(HashMap::new())),
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
    })
}

#[tokio::test]
#[ignore = "requires native Docker, permission to bind a loopback socket and create containers/networks, and LITEBIN_NATIVE_TEST_IMAGE preloaded with /bin/sh and sleep"]
async fn live_remote_compose_background_dispatches_through_agent_http() {
    let image = native_test_image().unwrap();
    let project_id = unique_project_id("remote-compose-path");
    let state = live_orchestrator_state().await.unwrap();
    let agent_state = live_agent_state().await.unwrap();
    cleanup_live_project(&state, &project_id).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move {
        axum::serve(listener, litebin_agent::build_router(agent_state)).await
    });

    let result: anyhow::Result<()> = async {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO nodes
             (id, name, host, agent_port, status, fail_count, created_at, updated_at)
             VALUES ('test-remote', 'Test Remote', '127.0.0.1', ?, 'online', 0, ?, ?)",
        )
        .bind(i64::from(port))
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
        insert_compose_project(&state.db, &project_id, "test-remote", &image).await?;
        write_compose(&project_id, &image)?;
        state
            .node_clients
            .insert("test-remote".into(), Arc::new(reqwest::Client::new()));

        let _ = start_project(State(state.clone()), Path(project_id.clone()))
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let first_id = assert_background_compose_state(&state, &project_id).await?;

        let _ = recreate_project(
            State(state.clone()),
            Path(project_id.clone()),
            Some(axum::Json(RecreateRequest::default())),
        )
        .await
        .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let recreated_id = assert_background_compose_state(&state, &project_id).await?;
        anyhow::ensure!(recreated_id != first_id);

        let logs = project_logs(
            State(state.clone()),
            Path(project_id.clone()),
            Query(LogsQuery {
                tail: Some(50),
                service: Some(SERVICE_NAME.into()),
            }),
        )
        .await
        .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?
        .0;
        anyhow::ensure!(logs.service_name.as_deref() == Some(SERVICE_NAME));
        anyhow::ensure!(logs.lines.iter().any(|line| line.contains(LOG_MARKER)));

        let _ = stop_project(State(state.clone()), Path(project_id.clone()))
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if project(&state.db, &project_id)
                    .await
                    .is_ok_and(|project| project.status == ProjectStatus::Stopped)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?;
        let (_, stopped_status, _) = service_row(&state.db, &project_id).await?;
        anyhow::ensure!(stopped_status == "stopped");

        let _ = delete_project(State(state.clone()), Path(project_id.clone()))
            .await
            .map_err(|(status, error)| anyhow::anyhow!("{status}: {error}"))?;
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_one(&state.db)
            .await?;
        anyhow::ensure!(remaining == 0);
        anyhow::ensure!(
            state
                .docker
                .list_project_workload_containers(&project_id)
                .await?
                .is_empty()
        );
        Ok(())
    }
    .await;

    cleanup_live_project(&state, &project_id).await;
    server_task.abort();
    let _ = server_task.await;
    result.unwrap();
}
