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

// ── Multi-Service Types ──────────────────────────────────────────────────────

fn default_false() -> bool {
    false
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
    pub allow_docker_access: Option<bool>,
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

    // Ensure project directory exists
    ensure_project_dir_and_env(&req.project_id);
    {
        let mut meta = state.project_meta.write().unwrap();
        let entry = meta.entry(req.project_id.clone()).or_default();
        entry.is_background = req.is_background;
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

    let extra_env = read_project_env(&req.project_id);

    // Build compose run plan (parse, topo sort, detect public, map configs)
    let mut plan = match litebin_common::compose_run::build_compose_run_plan(
        &req.compose_yaml, &req.project_id, &extra_env, None,
    ) {
        Ok(p) => p,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: format!("invalid compose: {e}") }),
        ).into_response(),
    };

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

    // Apply allow_docker_access flag and inject docker-socket-proxy if enabled
    if req.allow_docker_access.unwrap_or(false) {
        for config in plan.configs.iter_mut() {
            config.allow_docker_access = true;
        }
        plan.inject_docker_proxy(&req.project_id);
        // Pre-pull the proxy image (skip if already local)
        if let Err(e) = state.docker.pull_image_with_opts("tecnativa/docker-socket-proxy", false).await {
            tracing::warn!(error = %e, "failed to pull docker-socket-proxy image");
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
    let target_set: Option<std::collections::HashSet<String>> = req.target_services.as_ref()
        .map(|ts| ts.iter().cloned().collect());
    if let Ok(all_containers) = state.docker.list_containers_by_prefix(&prefix).await {
        for cid in &all_containers {
            if let Some(ref targets) = target_set {
                // Partial redeploy: check if this container belongs to a target service
                if let Ok(inspect) = state.docker.inspect_container(cid).await {
                    if let Some(ref name) = inspect.name {
                        let trimmed = name.trim_start_matches('/');
                        if let Some(svc_name) = trimmed.strip_prefix(&prefix) {
                            if targets.contains(svc_name) {
                                let _ = state.docker.stop_container(cid).await;
                                let _ = state.docker.remove_container(cid).await;
                            }
                        }
                    }
                }
            } else {
                // Full deploy: remove all
                let _ = state.docker.stop_container(cid).await;
                let _ = state.docker.remove_container(cid).await;
            }
        }
    }

    // Ensure per-project network
    if let Err(e) = state.docker.ensure_project_network(&req.project_id, None).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to create project network: {e}") }),
        ).into_response();
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
    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    // Pre-check: warn if any service has docker.sock but allow_docker_access is disabled
    let mut warnings: Vec<String> = Vec::new();
    if !req.allow_docker_access.unwrap_or(false) {
        let has_sock = plan.configs.iter().any(|c| {
            c.binds.as_ref().map_or(false, |binds| {
                binds.iter().any(|b| {
                    let source = b.split(':').next().unwrap_or("");
                    source.ends_with("/docker.sock")
                })
            })
        });
        if has_sock {
            warnings.push("Docker socket mounts found but 'Allow Docker access' is disabled — socket will not be available".into());
        }
    }

    // Start services level by level — parallel within each level
    let mut results: Vec<ServiceRunResult> = Vec::new();
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
                            mapped_port: is_public.then_some(mapped_port),
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
                    // If the docker-socket-proxy was started, wait for it to be
                    // network-ready before starting the next level.
                    if r.service_name == litebin_common::types::DOCKER_PROXY_SERVICE {
                        if let Some(ref cid) = r.container_id {
                            if let Err(e) = state.docker.wait_for_network_ready(cid).await {
                                tracing::warn!(error = %e, "docker-socket-proxy network readiness timeout, continuing");
                            } else {
                                tracing::info!(container_id = %cid, "docker-socket-proxy is network-ready");
                            }
                        }
                    }
                    results.push(r)
                }
                Err(e) => {
                    tracing::error!(error = %e, "batch-run: service task panicked");
                }
            }
        }
    }

    // Rebuild local Caddy with all running containers
    if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
        tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
    }

    (StatusCode::OK, Json(BatchRunResponse { services: results, warnings })).into_response()
}
