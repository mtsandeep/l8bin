use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::body::to_bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use serde_json::Value;

use super::super::env::projects_dir;
use super::batch_run;
use super::rollback::FAIL_NEXT_PROXY_READINESS_CHECK;
use super::types::{BatchRunRequest, host_network_authorized};
use crate::config::Config;
use crate::{AgentState, ProjectMetaEntry, WakeGuard};

struct FileSnapshot {
    path: std::path::PathBuf,
    contents: Option<Vec<u8>>,
}

impl FileSnapshot {
    fn capture(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        let contents = std::fs::read(&path).ok();
        Self { path, contents }
    }
}

impl Drop for FileSnapshot {
    fn drop(&mut self) {
        match &self.contents {
            Some(contents) => {
                if let Some(parent) = self.path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&self.path, contents);
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

async fn live_state() -> anyhow::Result<AgentState> {
    let mut docker = litebin_common::docker::DockerManager::new("litebin-live-tests".into(), 128 * 1024 * 1024, 0.25)?;
    docker.detect_host_projects_dir().await;
    Ok(AgentState {
        config: Arc::new(Config {
            agent_port: 0,
            upload_port: 0,
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
        wake_locks: Arc::new(DashMap::<String, Arc<WakeGuard>>::new()),
        registration: Arc::new(RwLock::new(None)),
        last_caddy_config: Arc::new(RwLock::new(None)),
        project_meta: Arc::new(RwLock::new(HashMap::<String, ProjectMetaEntry>::new())),
        proxy_client: reqwest::Client::new(),
        multi_svc_health_check: Arc::new(DashMap::new()),
        upload_store: Arc::new(litebin_common::upload::UploadStore::new(
            std::env::temp_dir().join(format!("l8b-test-upload-{}", std::process::id())),
            litebin_common::upload::DEFAULT_CHUNK_SIZE,
        )?),
    })
}

fn request(project_id: &str, compose_yaml: String) -> BatchRunRequest {
    BatchRunRequest {
        project_id: project_id.into(),
        compose_yaml,
        service_order: vec!["collector".into()],
        target_services: None,
        allow_raw_ports: Some(false),
        docker_observe: Some(false),
        host_network: Some(false),
        is_background: true,
        force_pull: false,
        stage_only: false,
        service_resources: None,
        default_memory_limit_mb: None,
        default_cpu_limit: None,
    }
}

async fn response_json(response: Response) -> anyhow::Result<(axum::http::StatusCode, Value)> {
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await?;
    let value = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body)? };
    Ok((status, value))
}

fn service_container_id<'a>(body: &'a Value, service: &str) -> anyhow::Result<&'a str> {
    body["services"]
        .as_array()
        .and_then(|services| services.iter().find(|entry| entry["service_name"] == service))
        .and_then(|entry| entry["container_id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("missing container id for service {service}: {body}"))
}

async fn cleanup_live_project(state: &AgentState, project_id: &str) {
    let _ = state.docker.cleanup_project_resources(project_id, &[]).await;
    let _ = std::fs::remove_dir_all(projects_dir().join(project_id));
}

#[test]
fn host_network_requires_grant_and_background_project() {
    assert!(host_network_authorized(true, true));
    assert!(!host_network_authorized(false, true));
    assert!(!host_network_authorized(true, false));
    assert!(!host_network_authorized(false, false));
}

#[tokio::test]
#[ignore = "requires a rootful Linux Docker engine with host networking, /var/run/docker.sock, registry access, and free loopback ports"]
async fn live_background_host_observer_runs_through_batch_handler_and_recreates() {
    let _meta_snapshot = FileSnapshot::capture("data/project-meta.json");
    let project_id = format!("live-host-observer-{}", std::process::id());
    let state = live_state().await.unwrap();
    cleanup_live_project(&state, &project_id).await;

    let result: anyhow::Result<()> = async {
        let host = state.docker.host_info().await?;
        litebin_common::docker::require_host_network_eligible(host.rootless, Some(3))?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let listen_port = listener.local_addr()?.port();
        drop(listener);
        let compose = format!(
            r#"services:
  collector:
    image: alpine:3.20
    network_mode: host
    environment:
      OUTBOUND_URL: https://receiver.invalid/v1/events
      LISTEN: "{listen_port}"
    command:
      - /bin/sh
      - -c
      - 'echo "$OUTBOUND_URL" > /var/lib/generic-agent/outbound; echo "$DOCKER_HOST" > /var/lib/generic-agent/docker-host; test -f /var/lib/generic-agent/persisted || echo retained > /var/lib/generic-agent/persisted; exec httpd -f -p "$LISTEN"'
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./agent-data:/var/lib/generic-agent
"#
        );
        let mut req = request(&project_id, compose);
        req.docker_observe = Some(true);
        req.host_network = Some(true);

        let (status, first) =
            response_json(batch_run(State(state.clone()), Json(req)).await.into_response())
                .await?;
        anyhow::ensure!(status.is_success(), "batch-run failed: {status} {first}");
        let workload_id = service_container_id(&first, "collector")?.to_string();
        let proxy_id = service_container_id(
            &first,
            litebin_common::types::DOCKER_PROXY_SERVICE,
        )?
        .to_string();

        let inspect = state.docker.inspect_container(&workload_id).await?;
        anyhow::ensure!(
            inspect
                .host_config
                .as_ref()
                .and_then(|config| config.network_mode.as_deref())
                == Some("host"),
            "workload did not use host networking"
        );
        anyhow::ensure!(
            inspect
                .mounts
                .as_ref()
                .is_some_and(|mounts| mounts.iter().all(|mount| {
                    mount.destination.as_deref() != Some("/var/run/docker.sock")
                })),
            "workload retained the raw Docker socket"
        );
        let env = inspect
            .config
            .as_ref()
            .and_then(|config| config.env.as_ref())
            .ok_or_else(|| anyhow::anyhow!("workload env missing"))?;
        anyhow::ensure!(
            env.iter()
                .any(|entry| entry == "OUTBOUND_URL=https://receiver.invalid/v1/events"),
            "outbound URL env missing"
        );
        anyhow::ensure!(
            env.iter().any(|entry| entry == &format!("LISTEN={listen_port}")),
            "LISTEN env missing"
        );
        let docker_host = env
            .iter()
            .find(|entry| entry.starts_with("DOCKER_HOST=tcp://127.0.0.1:"))
            .ok_or_else(|| anyhow::anyhow!("resolved loopback DOCKER_HOST missing"))?;
        let proxy_port: u16 = docker_host
            .rsplit(':')
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid DOCKER_HOST"))?
            .parse()?;
        anyhow::ensure!(
            inspect
                .host_config
                .as_ref()
                .and_then(|config| config.port_bindings.as_ref())
                .is_none(),
            "host workload unexpectedly has published ports"
        );

        let proxy = state.docker.inspect_container(&proxy_id).await?;
        let observe_network =
            litebin_common::types::docker_observe_network_name(&project_id, None);
        anyhow::ensure!(
            proxy
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                .is_some_and(|networks| {
                    networks.len() == 1 && networks.contains_key(&observe_network)
                }),
            "proxy was not isolated on its private bridge"
        );
        let binding = proxy
            .host_config
            .as_ref()
            .and_then(|config| config.port_bindings.as_ref())
            .and_then(|bindings| bindings.get("2375/tcp"))
            .and_then(|bindings| bindings.as_ref())
            .and_then(|bindings| bindings.first())
            .ok_or_else(|| anyhow::anyhow!("proxy loopback mapping missing"))?;
        anyhow::ensure!(
            binding.host_ip.as_deref() == Some("127.0.0.1"),
            "proxy mapping was not loopback-only"
        );

        let client = reqwest::Client::new();
        let version = client
            .get(format!("http://127.0.0.1:{proxy_port}/version"))
            .send()
            .await?;
        let mutation = client
            .post(format!(
                "http://127.0.0.1:{proxy_port}/containers/create"
            ))
            .send()
            .await?;
        anyhow::ensure!(version.status().is_success(), "observation read was denied");
        anyhow::ensure!(
            mutation.status() == reqwest::StatusCode::FORBIDDEN,
            "Docker mutation was not denied: {}",
            mutation.status()
        );
        let listener_response = client
            .get(format!("http://127.0.0.1:{listen_port}/"))
            .send()
            .await?;
        anyhow::ensure!(
            listener_response.status().is_client_error()
                || listener_response.status().is_success(),
            "host listener was not reachable"
        );

        let data_dir = projects_dir()
            .join(&project_id)
            .join("agent-data");
        anyhow::ensure!(
            std::fs::read_to_string(data_dir.join("persisted"))?.trim() == "retained",
            "relative bind did not persist data"
        );
        anyhow::ensure!(
            std::fs::read_to_string(data_dir.join("docker-host"))?.trim()
                == docker_host.trim_start_matches("DOCKER_HOST="),
            "workload did not receive the resolved Docker endpoint"
        );

        let mut recreate = request(
            &project_id,
            litebin_common::docker::DockerManager::read_compose(&project_id)
                .ok_or_else(|| anyhow::anyhow!("stored compose missing"))?,
        );
        recreate.docker_observe = Some(true);
        recreate.host_network = Some(true);
        let (status, recreated) = response_json(
            batch_run(State(state.clone()), Json(recreate))
                .await
                .into_response(),
        )
        .await?;
        anyhow::ensure!(
            status.is_success(),
            "recreate batch-run failed: {status} {recreated}"
        );
        anyhow::ensure!(
            service_container_id(&recreated, "collector")? != workload_id,
            "recreate reused the old workload identity"
        );
        anyhow::ensure!(
            std::fs::read_to_string(data_dir.join("persisted"))?.trim() == "retained",
            "bind data was lost across recreate"
        );
        Ok(())
    }
    .await;

    cleanup_live_project(&state, &project_id).await;
    result.unwrap();
    assert!(state.docker.list_project_workload_containers(&project_id).await.unwrap().is_empty());
    assert!(state.docker.current_docker_observe_proxy(&project_id).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires a local Docker daemon, registry access, and permission to create containers and networks"]
async fn live_one_service_compose_uses_agent_lifecycle_and_log_handlers() {
    let _meta_snapshot = FileSnapshot::capture("data/project-meta.json");
    let project_id = format!("live-one-service-{}", std::process::id());
    let state = live_state().await.unwrap();
    cleanup_live_project(&state, &project_id).await;

    let result: anyhow::Result<()> = async {
        let compose = r#"services:
  collector:
    image: alpine:3.20
    command: ["/bin/sh", "-c", "echo pathway-ready; exec sleep 300"]
"#
        .to_string();
        let (status, started) = response_json(
            batch_run(State(state.clone()), Json(request(&project_id, compose.clone()))).await.into_response(),
        )
        .await?;
        anyhow::ensure!(status.is_success(), "initial batch-run failed: {started}");
        let first_id = service_container_id(&started, "collector")?.to_string();

        let logs = crate::routes::containers::container_logs(
            State(state.clone()),
            Path(first_id.clone()),
            Query(super::super::types::LogsQuery { tail: Some(20) }),
        )
        .await
        .into_response();
        let log_status = logs.status();
        let log_body = to_bytes(logs.into_body(), 1024 * 1024).await?;
        anyhow::ensure!(log_status.is_success(), "log handler failed");
        anyhow::ensure!(
            String::from_utf8_lossy(&log_body).contains("pathway-ready"),
            "production log path omitted service output"
        );

        let (wrong_status, wrong_body) = response_json(
            crate::routes::containers::stop_service(
                State(state.clone()),
                Json(super::super::types::StopServiceRequest {
                    project_id: format!("{project_id}-other"),
                    service_name: "collector".into(),
                }),
            )
            .await
            .into_response(),
        )
        .await?;
        anyhow::ensure!(wrong_status.is_success() && wrong_body["stopped"] == false);
        anyhow::ensure!(
            state.docker.is_container_running(&first_id).await?,
            "identity-mismatched stop affected the workload"
        );

        let (stop_status, stop_body) = response_json(
            crate::routes::containers::stop_service(
                State(state.clone()),
                Json(super::super::types::StopServiceRequest {
                    project_id: project_id.clone(),
                    service_name: "collector".into(),
                }),
            )
            .await
            .into_response(),
        )
        .await?;
        anyhow::ensure!(stop_status.is_success() && stop_body["stopped"] == true);
        anyhow::ensure!(!state.docker.is_container_running(&first_id).await?);

        let (recreate_status, recreated) =
            response_json(batch_run(State(state.clone()), Json(request(&project_id, compose))).await.into_response())
                .await?;
        anyhow::ensure!(recreate_status.is_success(), "recreate failed: {recreated}");
        let second_id = service_container_id(&recreated, "collector")?.to_string();
        anyhow::ensure!(second_id != first_id, "recreate retained old container id");

        let (project_stop_status, project_stop) = response_json(
            crate::routes::containers::stop_project(
                State(state.clone()),
                Json(super::super::types::StopProjectRequest { project_id: project_id.clone() }),
            )
            .await
            .into_response(),
        )
        .await?;
        anyhow::ensure!(
            project_stop_status.is_success() && project_stop["stopped_containers"] == 1,
            "stop-project metadata was incorrect: {project_stop}"
        );
        anyhow::ensure!(!state.docker.is_container_running(&second_id).await?);

        let cleanup = crate::routes::containers::cleanup_project(
            State(state.clone()),
            Json(super::super::types::CleanupRequest { project_id: project_id.clone(), volumes: Vec::new() }),
        )
        .await
        .into_response();
        anyhow::ensure!(cleanup.status().is_success(), "cleanup handler failed");
        anyhow::ensure!(
            state.docker.list_project_workload_containers(&project_id).await?.is_empty(),
            "delete cleanup left project workloads"
        );
        Ok(())
    }
    .await;

    cleanup_live_project(&state, &project_id).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a local Docker daemon, registry access, and permission to create containers and networks"]
async fn live_proxy_readiness_failure_rolls_back_proxy_and_workload() {
    let _meta_snapshot = FileSnapshot::capture("data/project-meta.json");
    let project_id = format!("live-proxy-rollback-{}", std::process::id());
    let state = live_state().await.unwrap();
    cleanup_live_project(&state, &project_id).await;

    let result: anyhow::Result<()> = async {
        let compose = r#"services:
  collector:
    image: alpine:3.20
    command: ["/bin/sh", "-c", "exec sleep 300"]
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
"#
        .to_string();
        let mut req = request(&project_id, compose);
        req.docker_observe = Some(true);
        FAIL_NEXT_PROXY_READINESS_CHECK.store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, body) = response_json(batch_run(State(state.clone()), Json(req)).await.into_response()).await?;
        FAIL_NEXT_PROXY_READINESS_CHECK.store(false, std::sync::atomic::Ordering::SeqCst);
        anyhow::ensure!(
            status == axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "faulted proxy start unexpectedly succeeded: {body}"
        );
        anyhow::ensure!(
            body["error"].as_str().is_some_and(|error| error.contains("proxy failed health check")),
            "failure metadata did not identify proxy readiness: {body}"
        );
        anyhow::ensure!(
            state.docker.current_docker_observe_proxy(&project_id).await?.is_none(),
            "failed proxy was not rolled back"
        );
        anyhow::ensure!(
            state.docker.list_project_workload_containers(&project_id).await?.is_empty(),
            "workload started despite failed proxy"
        );
        Ok(())
    }
    .await;

    FAIL_NEXT_PROXY_READINESS_CHECK.store(false, std::sync::atomic::Ordering::SeqCst);
    cleanup_live_project(&state, &project_id).await;
    result.unwrap();
}
