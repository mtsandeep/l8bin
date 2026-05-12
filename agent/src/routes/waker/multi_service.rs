use std::sync::Arc;
use tokio::task::JoinSet;

use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::AgentState;

use super::caddy::rebuild_local_caddy;

type HmacSha256 = Hmac<Sha256>;

/// Wake a multi-service project: read compose.yaml, parse topological order,
/// start all service containers in dependency order, rebuild Caddy, report to master.
pub(super) async fn wake_multi_service(state: &AgentState, project_id: &str) -> anyhow::Result<()> {
    // Read compose.yaml from disk
    let compose_yaml = match litebin_common::docker::DockerManager::read_compose(project_id) {
        Some(yaml) => yaml,
        None => {
            anyhow::bail!("no compose.yaml found for multi-service project {}", project_id);
        }
    };

    let extra_env = crate::routes::containers::read_project_env(project_id);

    let mut plan = litebin_common::compose_run::build_compose_run_plan(
        &compose_yaml, project_id, &extra_env, None,
    )?;

    // Apply allow_raw_ports flag from agent state
    let allow_raw = state.project_meta.read().unwrap()
        .get(project_id).map(|e| e.allow_raw_ports).unwrap_or(false);
    for config in plan.configs.iter_mut() {
        config.allow_raw_ports = allow_raw;
    }

    // Apply allow_docker_access flag and inject docker-socket-proxy if enabled
    let allow_docker = state.project_meta.read().unwrap()
        .get(project_id).map(|e| e.allow_docker_access).unwrap_or(false);
    for config in plan.configs.iter_mut() {
        config.allow_docker_access = allow_docker;
    }
    if allow_docker {
        plan.inject_docker_proxy(project_id);
        // Pre-pull the proxy image (skip if already local)
        if let Err(e) = state.docker.pull_image_with_opts("tecnativa/docker-socket-proxy", false).await {
            tracing::warn!(error = %e, "failed to pull docker-socket-proxy image");
        }
    }

    // Apply global defaults for services without explicit limits
    if let Some(entry) = state.project_meta.read().unwrap().get(project_id) {
        for config in plan.configs.iter_mut() {
            if config.memory_limit_mb.is_none() {
                config.memory_limit_mb = entry.default_memory_limit_mb;
            }
            if config.cpu_limit.is_none() {
                config.cpu_limit = entry.default_cpu_limit;
            }
        }
    }

    tracing::info!(
        project_id = %project_id,
        services = ?plan.service_order,
        "wake_multi_service: starting services in dependency order"
    );

    // Ensure per-project network
    state.docker.ensure_project_network(project_id, None).await?;

    // Connect Caddy to the project network
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(project_id, None);
    let _ = state.docker.connect_container_to_network(&caddy_container, &project_network).await;

    // Connect agent to the project network so it can proxy to containers
    let agent_container = std::env::var("AGENT_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-agent".into());
    let _ = state.docker.connect_container_to_network(&agent_container, &project_network).await;

    // Build owned lookup: service_name -> RunServiceConfig
    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    let mut public_container_id: Option<String> = None;
    let mut public_mapped_port: Option<u16> = None;
    let any_created = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Start services level by level — parallel within each level
    for level in &plan.service_levels {
        let mut tasks: JoinSet<Result<(String, u16, bool), String>> = JoinSet::new();

        for svc_name in level {
            let run_config = configs_map[svc_name].clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let is_public = run_config.is_public;
            let pid = project_id.to_string();
            let any_created = any_created.clone();

            tasks.spawn(async move {
                // Check if container already exists and is running
                let cname = litebin_common::types::container_name(&pid, &svc, None);
                if let Ok(Some(existing_id)) = docker.find_container_by_name(&cname).await {
                    if docker.is_container_running(&existing_id).await.unwrap_or(false) {
                        tracing::info!(
                            project_id = %pid,
                            service = %svc,
                            "wake_multi_service: service already running, skipping"
                        );
                        // Return existing container info so public service tracking still works
                        let port = run_config.port.unwrap_or(80) as u16;
                        return Ok((existing_id, port, is_public));
                    }
                    // Container exists but is stopped — just start it (fast path)
                    docker.start_existing_container(&existing_id).await
                        .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;
                    tracing::info!(
                        project_id = %pid,
                        service = %svc,
                        container = %existing_id,
                        "wake_multi_service: started existing stopped container"
                    );
                    let port = run_config.port.unwrap_or(80) as u16;
                    return Ok((existing_id, port, is_public));
                }

                let (container_id, mapped_port) = docker.run_service_container(&run_config).await
                    .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;

                any_created.store(true, std::sync::atomic::Ordering::Relaxed);

                tracing::info!(
                    project_id = %pid,
                    service = %svc,
                    container = %container_id,
                    port = %mapped_port,
                    "wake_multi_service: service created"
                );

                Ok((container_id, mapped_port, is_public))
            });
        }

        // Collect results from this level
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((container_id, mapped_port, is_public))) => {
                    if is_public {
                        public_container_id = Some(container_id.clone());
                        public_mapped_port = Some(mapped_port);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "wake_multi_service: failed to start service");
                    anyhow::bail!("{}", e);
                }
                Err(e) => {
                    tracing::error!(error = %e, "wake_multi_service: service task panicked");
                    anyhow::bail!("service task panicked");
                }
            }
        }
    }

    // Rebuild local Caddy with all running containers
    rebuild_local_caddy(state).await?;

    // Report to master with public service info
    if let (Some(cid), Some(port)) = (public_container_id, public_mapped_port) {
        report_wake_to_master(state, project_id, &cid, port).await;
    }

    tracing::info!(project_id = %project_id, "wake_multi_service: all services started");

    // Wait for Docker DNS to propagate only if we created new containers.
    if any_created.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    Ok(())
}

/// Best-effort report to orchestrator about a successful wake.
/// Fire-and-forget — if master is down, this silently fails.
/// Requests are HMAC-signed so the orchestrator can verify authenticity.
pub(super) async fn report_wake_to_master(
    state: &AgentState,
    project_id: &str,
    container_id: &str,
    mapped_port: u16,
) {
    let reg = match state.registration.read().unwrap().clone() {
        Some(r) => r,
        None => {
            tracing::debug!(project_id, "skipping wake report: agent not registered");
            return;
        }
    };

    let url = reg.wake_report_url;
    let node_id = &reg.node_id;
    let secret = &reg.secret;

    let timestamp = chrono::Utc::now().timestamp();
    let message = format!("{}\n{}", timestamp, node_id);

    // Compute HMAC-SHA256(secret, "{timestamp}\n{node_id}")
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create HMAC");
            return;
        }
    };
    mac.update(message.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    let body = json!({
        "project_id": project_id,
        "container_id": container_id,
        "mapped_port": mapped_port,
    });

    // Fire-and-forget: use a short timeout and ignore errors
    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Agent-Id", node_id.as_str())
        .header("X-Agent-Timestamp", timestamp.to_string())
        .header("X-Agent-Signature", signature)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(project_id, "wake report accepted by orchestrator");
        }
        Ok(resp) => {
            tracing::debug!(
                project_id,
                status = %resp.status(),
                "wake report rejected by orchestrator"
            );
        }
        Err(e) => {
            tracing::debug!(
                project_id,
                error = %e,
                "wake report failed (orchestrator may be down)"
            );
        }
    }
}
