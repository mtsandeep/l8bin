use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::AgentState;

use super::env::{ensure_project_dir_and_env, projects_dir, read_project_env};
use super::types::ErrorResponse;

#[cfg(test)]
static FAIL_NEXT_PROXY_READINESS_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

async fn wait_for_proxy_ready(
    docker: &litebin_common::docker::DockerManager,
    container_id: &str,
) -> anyhow::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_PROXY_READINESS_CHECK.swap(false, std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("test-injected proxy readiness failure");
    }
    docker.wait_for_healthy(container_id, true).await
}

// ── Multi-Service Types ──────────────────────────────────────────────────────

fn default_false() -> bool {
    false
}

fn host_network_authorized(granted: bool, is_background: bool) -> bool {
    granted && is_background
}

#[derive(Deserialize)]
pub struct BatchRunRequest {
    pub project_id: String,
    pub compose_yaml: String,
    /// Ordered list of service names to start (topologically sorted by orchestrator).
    pub service_order: Vec<String>,
    /// If Some, only recreate these services (partial redeploy). If None, deploy all.
    pub target_services: Option<Vec<String>>,
    pub allow_raw_ports: Option<bool>,
    pub docker_observe: Option<bool>,
    pub host_network: Option<bool>,
    #[serde(default = "default_false")]
    pub is_background: bool,
    /// Whether to force-pull images (true) or skip if already present locally (false).
    #[serde(default = "default_false")]
    pub force_pull: bool,
    /// When true, only persist compose.yaml and create the runtime `.env` placeholder.
    /// No networks, pulls, or containers are started.
    #[serde(default = "default_false")]
    pub stage_only: bool,
    /// Per-service resource overrides from dashboard (service_name → {memory_limit_mb, cpu_limit}).
    /// Applied on top of compose-embedded limits; None values mean "use global default".
    pub service_resources: Option<std::collections::HashMap<String, ServiceResources>>,
    /// Global default memory limit (MB) from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a memory limit.
    pub default_memory_limit_mb: Option<i64>,
    /// Global default CPU limit from orchestrator settings. Used when neither
    /// the compose YAML nor per-service overrides specify a CPU limit.
    pub default_cpu_limit: Option<f64>,
}

/// Per-service resource overrides sent by the orchestrator.
#[derive(Deserialize)]
pub struct ServiceResources {
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
}

#[derive(Serialize)]
pub struct BatchRunResponse {
    pub services: Vec<ServiceRunResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ServiceRunResult {
    pub service_name: String,
    pub container_id: Option<String>,
    pub mapped_port: Option<u16>,
    pub error: Option<String>,
}

#[derive(Serialize)]
struct BatchRunErrorResponse {
    error: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    affected_services: Vec<String>,
}

fn batch_run_error(
    status: StatusCode,
    error: impl Into<String>,
    affected_services: &[String],
) -> axum::response::Response {
    let mut affected_services = affected_services.to_vec();
    affected_services.sort();
    affected_services.dedup();
    (
        status,
        Json(BatchRunErrorResponse {
            error: error.into(),
            affected_services,
        }),
    )
        .into_response()
}

async fn rollback_started_containers(
    docker: &litebin_common::docker::DockerManager,
    project_id: &str,
    service_names: &[String],
    container_ids: &[String],
) {
    for container_id in container_ids.iter().rev() {
        let _ = docker.stop_container(container_id).await;
        let _ = docker.remove_container(container_id).await;
    }
    for service_name in service_names {
        let _ = docker
            .remove_by_service_name(project_id, service_name, None)
            .await;
    }
}

/// POST /containers/batch-run
/// Deploy a multi-service project on the agent: store compose, pull images, start in order.
pub async fn batch_run(
    State(state): State<AgentState>,
    Json(req): Json<BatchRunRequest>,
) -> impl IntoResponse {
    tracing::info!(
        project = %req.project_id,
        services = ?req.service_order,
        stage_only = req.stage_only,
        "batch-run request received"
    );

    // Parse and authorize host networking before any filesystem, metadata, network,
    // or container mutation at the agent trust boundary.
    let compatibility = match compose_bollard::analyze_compose_yaml_for_workload(
        &req.compose_yaml,
        None,
        Some(&req.project_id),
        req.is_background,
    ) {
        Ok((_, report)) => report,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid compose: {error}"),
                }),
            )
                .into_response()
        }
    };
    if !compatibility.ok {
        let reasons = compatibility
            .unsupported()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("unsupported compose configuration: {reasons}"),
            }),
        )
            .into_response();
    }
    let extra_env = read_project_env(&req.project_id);
    let mut plan = match litebin_common::compose_run::build_compose_run_plan(
        &req.compose_yaml, &req.project_id, &extra_env, None,
    ) {
        Ok(plan) => plan,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: format!("invalid compose: {e}") }),
        ).into_response(),
    };
    let requests_host_network = plan.configs.iter().any(|config| config.host_network);
    if requests_host_network {
        if !req.host_network.unwrap_or(false) {
            return (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse { error: "host-network capability was not authorized".into() }),
            ).into_response();
        }
        if !host_network_authorized(req.host_network.unwrap_or(false), req.is_background) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse { error: "host networking is restricted to background projects".into() }),
            ).into_response();
        }
        let host = state.docker.host_info().await.ok();
        if let Err(error) = litebin_common::docker::require_host_network_eligible(
            host.as_ref().and_then(|info| info.os_type.as_deref()),
            host.as_ref()
                .and_then(|info| info.operating_system.as_deref()),
            host.as_ref().and_then(|info| info.rootless),
            Some(3),
        ) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: error.to_string() }),
            ).into_response();
        }
    }

    // Ensure project directory exists
    ensure_project_dir_and_env(&req.project_id);
    {
        let mut meta = state.project_meta.write().unwrap();
        let entry = meta.entry(req.project_id.clone()).or_default();
        entry.is_background = req.is_background;
        entry.docker_observe = req.docker_observe.unwrap_or(false);
        entry.host_network = req.host_network.unwrap_or(false);
        if req.is_background {
            entry.auto_start_enabled = false;
        }
    }
    crate::save_project_meta_to_file(&state.project_meta.read().unwrap());

    // Store compose.yaml
    let compose_path = projects_dir().join(&req.project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &req.compose_yaml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to store compose.yaml: {e}") }),
        ).into_response();
    }

    // First-deploy staging: prepare runtime files without starting anything.
    if req.stage_only {
        tracing::info!(project = %req.project_id, "batch-run staged (no containers started)");
        return (
            StatusCode::OK,
            Json(BatchRunResponse {
                services: Vec::new(),
                warnings: Vec::new(),
            }),
        ).into_response();
    }

    if req.is_background {
        plan.pub_service_name = None;
        for config in plan.configs.iter_mut() {
            config.is_public = false;
        }
    }

    // Apply allow_raw_ports flag from orchestrator
    if req.allow_raw_ports.unwrap_or(false) {
        for config in plan.configs.iter_mut() {
            config.allow_raw_ports = true;
        }
    }

    // Inject the read-only observation proxy only for the explicit new capability.
    let docker_observe = req.docker_observe.unwrap_or(false);
    let proxy_injected = if docker_observe {
        match plan.inject_docker_observe_proxy(&req.project_id) {
            Ok(injected) => injected,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("failed to configure Docker observation proxy: {e}") }),
                ).into_response();
            }
        }
    } else {
        false
    };
    let mut target_set: Option<std::collections::HashSet<String>> = req
        .target_services
        .as_ref()
        .map(|services| services.iter().cloned().collect());
    let host_observers = plan.host_docker_observer_names();
    let current_proxy = if proxy_injected {
        match state
            .docker
            .current_docker_observe_proxy(&req.project_id)
            .await
        {
            Ok(proxy) => proxy,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("failed to inspect Docker observation proxy: {e}") }),
                ).into_response();
            }
        }
    } else {
        None
    };
    // Reuse is deliberately limited to partial operations. A full start/deploy
    // recreates host observers so no stopped container can retain stale env.
    let reusable_proxy = target_set.is_some()
        && current_proxy
            .as_ref()
            .is_some_and(|(_, port)| host_observers.is_empty() || port.is_some());
    if reusable_proxy {
        if let Some((_, Some(port))) = current_proxy {
            plan.inject_host_docker_proxy_endpoint(port);
        }
        plan.reuse_existing_docker_observe_proxy();
        if let Some(ref mut targets) = target_set {
            targets.remove(litebin_common::types::DOCKER_PROXY_SERVICE);
        }
    } else if proxy_injected {
        if let Some(ref mut targets) = target_set {
            plan.expand_for_docker_proxy_replacement(targets);
        }
    } else if target_set.is_none() {
        if let Err(e) = state
            .docker
            .remove_by_service_name(
                &req.project_id,
                litebin_common::types::DOCKER_PROXY_SERVICE,
                None,
            )
            .await
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to clean up previous Docker observation proxy: {e}") }),
            ).into_response();
        }
    }
    let proxy_created = proxy_injected && !reusable_proxy;
    if proxy_created {
        if let Err(e) = state.docker.pull_image_with_opts(litebin_common::types::DOCKER_OBSERVE_PROXY_IMAGE, false).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to prepare Docker observation proxy: {e}") }),
            ).into_response();
        }
    } else if !proxy_injected && target_set.is_none() {
        let network = litebin_common::types::docker_observe_network_name(&req.project_id, None);
        if let Err(e) = state.docker.remove_named_network(&network).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to remove Docker observation network: {e}") }),
            ).into_response();
        }
    }

    // Apply per-service resource overrides from orchestrator (dashboard-set memory/CPU)
    if let Some(ref overrides) = req.service_resources {
        for config in plan.configs.iter_mut() {
            if let Some(res) = overrides.get(&config.service_name) {
                if res.memory_limit_mb.is_some() {
                    config.memory_limit_mb = res.memory_limit_mb;
                }
                if res.cpu_limit.is_some() {
                    config.cpu_limit = res.cpu_limit;
                }
            }
        }
    }

    // Apply global defaults for services that still have no explicit limit
    if req.default_memory_limit_mb.is_some() || req.default_cpu_limit.is_some() {
        for config in plan.configs.iter_mut() {
            if config.memory_limit_mb.is_none() {
                config.memory_limit_mb = req.default_memory_limit_mb;
            }
            if config.cpu_limit.is_none() {
                config.cpu_limit = req.default_cpu_limit;
            }
        }
    }

    // Clean up existing containers from a previous deploy (by name prefix)
    // On partial redeploy, only remove targeted service containers
    let prefix = format!("litebin-{}.", req.project_id);
    let all_containers = match state.docker.list_containers_by_prefix(&prefix).await {
        Ok(containers) => containers,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to list existing project containers: {e}") }),
            ).into_response();
        }
    };
    let mut removed_services = Vec::new();
    for cid in &all_containers {
        let inspect = match state.docker.inspect_container(cid).await {
            Ok(inspect) => inspect,
            Err(e) => {
                return batch_run_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to inspect existing project container: {e}"),
                    &removed_services,
                );
            }
        };
        let service_name = inspect
            .name
            .as_deref()
            .map(|name| name.trim_start_matches('/'))
            .and_then(|name| name.strip_prefix(&prefix))
            .map(str::to_owned);
        let should_remove = target_set
            .as_ref()
            .is_none_or(|targets| service_name.as_ref().is_some_and(|service| targets.contains(service)));
        if should_remove {
            if let Err(e) = state.docker.remove_container(cid).await {
                return batch_run_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to remove existing project container: {e}"),
                    &removed_services,
                );
            }
            if let Some(service_name) = service_name {
                removed_services.push(service_name);
            }
        }
    }

    // Ensure per-project network
    if let Err(e) = state.docker.ensure_project_network(&req.project_id, None).await {
        return batch_run_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create project network: {e}"),
            &removed_services,
        );
    }
    if proxy_injected {
        let network = litebin_common::types::docker_observe_network_name(&req.project_id, None);
        if let Err(e) = state.docker.ensure_named_network(&network).await {
            return batch_run_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create Docker observation network: {e}"),
                &removed_services,
            );
        }
    }

    // Connect Caddy to the project network
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(&req.project_id, None);
    let _ = state.docker.connect_container_to_network(&caddy_container, &project_network).await;

    // Pull images in parallel (only for target services on partial redeploy)
    // Skip sha256: images — they were pre-loaded via /images/upload, not from a registry.
    let images_to_pull: Vec<String> = if let Some(ref targets) = target_set {
        plan.configs.iter()
            .filter(|c| targets.contains(&c.service_name))
            .map(|c| c.image.clone())
            .collect()
    } else {
        plan.configs.iter().map(|c| c.image.clone()).collect()
    };
    let images_to_pull: Vec<String> = images_to_pull
        .into_iter()
        .filter(|img| !img.starts_with("sha256:"))
        .collect();
    let force_pull = req.force_pull;
    let pull_handles: Vec<_> = images_to_pull.into_iter().map(|image| {
        let docker = state.docker.clone();
        tokio::spawn(async move {
            (image.clone(), docker.pull_image_with_opts(&image, force_pull).await.map_err(|e| e.to_string()))
        })
    }).collect();

    for handle in pull_handles {
        if let Ok((image, result)) = handle.await {
            if let Err(e) = result {
                tracing::error!(image = %image, error = %e, "batch-run: failed to pull image");
            }
        }
    }

    // Build owned lookup: service_name -> RunServiceConfig
    let mut configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    // Pre-check: warn if a socket declaration has no explicit observation grant.
    let mut warnings: Vec<String> = Vec::new();
    if !docker_observe {
        let has_sock = plan.configs.iter().any(|c| {
            c.binds.as_ref().map_or(false, |binds| {
                binds.iter().any(|b| {
                    let source = b.split(':').next().unwrap_or("");
                    source.ends_with("/docker.sock")
                })
            })
        });
        if has_sock {
            warnings.push("Docker socket declaration found without docker-observe — the raw socket was removed".into());
        }
    }

    // Start services level by level — parallel within each level
    let mut results: Vec<ServiceRunResult> = Vec::new();
    let mut started_container_ids: Vec<String> = Vec::new();
    let operation_services: Vec<String> = plan
        .service_order
        .iter()
        .filter(|service| target_set.as_ref().is_none_or(|targets| targets.contains(*service)))
        .cloned()
        .collect();
    for level in &plan.service_levels {
        let mut tasks: JoinSet<ServiceRunResult> = JoinSet::new();

        for svc_name in level {
            // Apply target filter for partial redeploy
            if let Some(ref targets) = target_set {
                if !targets.contains(svc_name) {
                    continue;
                }
            }

            let run_config = configs_map[svc_name].clone();
            let is_public = run_config.is_public;
            let is_proxy = run_config.is_managed_docker_proxy;
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let pid = req.project_id.clone();

            tasks.spawn(async move {
                match docker.run_service_container(&run_config).await {
                    Ok((container_id, mapped_port)) => {
                        tracing::info!(
                            project = %pid,
                            service = %svc,
                            container = %container_id,
                            port = %mapped_port,
                            "batch-run: service started"
                        );
                        ServiceRunResult {
                            service_name: svc,
                            container_id: Some(container_id),
                            mapped_port: (is_public || is_proxy).then_some(mapped_port),
                            error: None,
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            project = %pid,
                            service = %svc,
                            error = %e,
                            "batch-run: failed to start service"
                        );
                        ServiceRunResult {
                            service_name: svc,
                            container_id: None,
                            mapped_port: None,
                            error: Some(e.to_string()),
                        }
                    }
                }
            });
        }

        // Collect results from this level
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(r) => {
                    // Observation access fails closed: workloads are not started
                    // unless the managed proxy becomes healthy.
                    if r.service_name == litebin_common::types::DOCKER_PROXY_SERVICE {
                        let Some(ref cid) = r.container_id else {
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
                            return batch_run_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Docker observation proxy failed to start",
                                &removed_services,
                            );
                        };
                        if let Err(e) = wait_for_proxy_ready(&state.docker, cid).await {
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                            rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
                            return batch_run_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Docker observation proxy failed health check: {e}"),
                                &removed_services,
                            );
                        }
                        if configs_map
                            .values()
                            .any(|config| config.host_network && config.docker_observe)
                        {
                            let port = match state
                                .docker
                                .inspect_mapped_port_for(cid, "2375/tcp")
                                .await
                            {
                                Ok(Some(port)) => port,
                                Ok(None) => {
                                    rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
                                    return batch_run_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "Docker observation proxy did not receive its required loopback mapping",
                                        &removed_services,
                                    );
                                }
                                Err(error) => {
                                    rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
                                    return batch_run_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("failed to inspect Docker observation proxy mapping: {error}"),
                                        &removed_services,
                                    );
                                }
                            };
                            for config in configs_map.values_mut() {
                                if config.host_network && config.docker_observe {
                                    config.env.retain(|value| !value.starts_with("DOCKER_HOST="));
                                    config.env.push(format!("DOCKER_HOST=tcp://127.0.0.1:{port}"));
                                }
                            }
                        }
                    }
                    if let Some(ref container_id) = r.container_id {
                        started_container_ids.push(container_id.clone());
                    }
                    results.push(r)
                }
                Err(e) => {
                    tracing::error!(error = %e, "batch-run: service task panicked");
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
                    return batch_run_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("service task failed: {e}"),
                        &removed_services,
                    );
                }
            }
        }
        let level_errors: Vec<String> = results
            .iter()
            .filter_map(|result| {
                result
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {}", result.service_name, error))
            })
            .collect();
        if !level_errors.is_empty() {
            rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids).await;
            return batch_run_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("one or more services failed: {}", level_errors.join("; ")),
                &removed_services,
            );
        }
    }

    // Rebuild local Caddy with all running containers
    if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
        tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
    }

    (StatusCode::OK, Json(BatchRunResponse { services: results, warnings })).into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use axum::body::to_bytes;
    use axum::extract::{Path, Query, State};
    use axum::response::{IntoResponse, Response};
    use axum::Json;
    use dashmap::DashMap;
    use serde_json::Value;

    use super::{
        BatchRunRequest, FAIL_NEXT_PROXY_READINESS_CHECK, batch_run, host_network_authorized,
    };
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
        let mut docker = litebin_common::docker::DockerManager::new(
            "litebin-live-tests".into(),
            128 * 1024 * 1024,
            0.25,
        )?;
        docker.detect_host_projects_dir().await;
        Ok(AgentState {
            config: Arc::new(Config {
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
            wake_locks: Arc::new(DashMap::<String, Arc<WakeGuard>>::new()),
            registration: Arc::new(RwLock::new(None)),
            last_caddy_config: Arc::new(RwLock::new(None)),
            project_meta: Arc::new(RwLock::new(HashMap::<String, ProjectMetaEntry>::new())),
            proxy_client: reqwest::Client::new(),
            multi_svc_health_check: Arc::new(DashMap::new()),
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
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)?
        };
        Ok((status, value))
    }

    fn service_container_id<'a>(body: &'a Value, service: &str) -> anyhow::Result<&'a str> {
        body["services"]
            .as_array()
            .and_then(|services| {
                services
                    .iter()
                    .find(|entry| entry["service_name"] == service)
            })
            .and_then(|entry| entry["container_id"].as_str())
            .ok_or_else(|| anyhow::anyhow!("missing container id for service {service}: {body}"))
    }

    async fn cleanup_live_project(state: &AgentState, project_id: &str) {
        let _ = state
            .docker
            .cleanup_project_resources(project_id, &[])
            .await;
        let _ = std::fs::remove_dir_all(
            std::path::PathBuf::from("projects").join(project_id),
        );
    }

    #[test]
    fn host_network_requires_grant_and_background_project() {
        assert!(host_network_authorized(true, true));
        assert!(!host_network_authorized(false, true));
        assert!(!host_network_authorized(true, false));
        assert!(!host_network_authorized(false, false));
    }

    #[tokio::test]
    #[ignore = "requires native Linux with rootful Docker, host networking, /var/run/docker.sock, registry access, and free loopback ports"]
    async fn live_background_host_observer_runs_through_batch_handler_and_recreates() {
        let _meta_snapshot = FileSnapshot::capture("data/project-meta.json");
        let project_id = format!("live-host-observer-{}", std::process::id());
        let state = live_state().await.unwrap();
        cleanup_live_project(&state, &project_id).await;

        let result: anyhow::Result<()> = async {
            let host = state.docker.host_info().await?;
            litebin_common::docker::require_host_network_eligible(
                host.os_type.as_deref(),
                host.operating_system.as_deref(),
                host.rootless,
                Some(3),
            )?;
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

            let data_dir = std::path::PathBuf::from("projects")
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
        assert!(state
            .docker
            .list_project_workload_containers(&project_id)
            .await
            .unwrap()
            .is_empty());
        assert!(state
            .docker
            .current_docker_observe_proxy(&project_id)
            .await
            .unwrap()
            .is_none());
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
                batch_run(State(state.clone()), Json(request(&project_id, compose.clone())))
                    .await
                    .into_response(),
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

            let (recreate_status, recreated) = response_json(
                batch_run(State(state.clone()), Json(request(&project_id, compose)))
                    .await
                    .into_response(),
            )
            .await?;
            anyhow::ensure!(
                recreate_status.is_success(),
                "recreate failed: {recreated}"
            );
            let second_id = service_container_id(&recreated, "collector")?.to_string();
            anyhow::ensure!(second_id != first_id, "recreate retained old container id");

            let (project_stop_status, project_stop) = response_json(
                crate::routes::containers::stop_project(
                    State(state.clone()),
                    Json(super::super::types::StopProjectRequest {
                        project_id: project_id.clone(),
                    }),
                )
                .await
                .into_response(),
            )
            .await?;
            anyhow::ensure!(
                project_stop_status.is_success()
                    && project_stop["stopped_containers"] == 1,
                "stop-project metadata was incorrect: {project_stop}"
            );
            anyhow::ensure!(!state.docker.is_container_running(&second_id).await?);

            let cleanup = crate::routes::containers::cleanup_project(
                State(state.clone()),
                Json(super::super::types::CleanupRequest {
                    project_id: project_id.clone(),
                    volumes: Vec::new(),
                }),
            )
            .await
            .into_response();
            anyhow::ensure!(cleanup.status().is_success(), "cleanup handler failed");
            anyhow::ensure!(
                state
                    .docker
                    .list_project_workload_containers(&project_id)
                    .await?
                    .is_empty(),
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
            FAIL_NEXT_PROXY_READINESS_CHECK
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let (status, body) =
                response_json(batch_run(State(state.clone()), Json(req)).await.into_response())
                    .await?;
            FAIL_NEXT_PROXY_READINESS_CHECK
                .store(false, std::sync::atomic::Ordering::SeqCst);
            anyhow::ensure!(
                status == axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "faulted proxy start unexpectedly succeeded: {body}"
            );
            anyhow::ensure!(
                body["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("proxy failed health check")),
                "failure metadata did not identify proxy readiness: {body}"
            );
            anyhow::ensure!(
                state
                    .docker
                    .current_docker_observe_proxy(&project_id)
                    .await?
                    .is_none(),
                "failed proxy was not rolled back"
            );
            anyhow::ensure!(
                state
                    .docker
                    .list_project_workload_containers(&project_id)
                    .await?
                    .is_empty(),
                "workload started despite failed proxy"
            );
            Ok(())
        }
        .await;

        FAIL_NEXT_PROXY_READINESS_CHECK.store(false, std::sync::atomic::Ordering::SeqCst);
        cleanup_live_project(&state, &project_id).await;
        result.unwrap();
    }
}
