use std::sync::Arc;
use tokio::task::JoinSet;

use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::AgentState;

use super::caddy::rebuild_local_caddy;

type HmacSha256 = Hmac<Sha256>;

async fn rollback_wake_containers(
    docker: &litebin_common::docker::DockerManager,
    container_ids: &Arc<std::sync::Mutex<Vec<String>>>,
) {
    let ids = container_ids.lock().map(|ids| ids.clone()).unwrap_or_default();
    for container_id in ids.iter().rev() {
        let _ = docker.stop_container(container_id).await;
        let _ = docker.remove_container(container_id).await;
    }
}

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
    let requests_host_network = plan.configs.iter().any(|config| config.host_network);
    if requests_host_network {
        let meta = state.project_meta.read().unwrap().get(project_id).cloned();
        if !meta.as_ref().is_some_and(|entry| entry.host_network && entry.is_background) {
            anyhow::bail!("host-network workload is not authorized as a background project");
        }
        let host = state.docker.host_info().await.ok();
        litebin_common::docker::require_host_network_eligible(
            host.as_ref().and_then(|info| info.os_type.as_deref()),
            host.as_ref()
                .and_then(|info| info.operating_system.as_deref()),
            host.as_ref().and_then(|info| info.rootless),
            Some(3),
        )?;
    }

    // Apply allow_raw_ports flag from agent state
    let allow_raw = state.project_meta.read().unwrap()
        .get(project_id).map(|e| e.allow_raw_ports).unwrap_or(false);
    for config in plan.configs.iter_mut() {
        config.allow_raw_ports = allow_raw;
    }

    // Inject read-only Docker observation only when explicitly granted.
    let docker_observe = state.project_meta.read().unwrap()
        .get(project_id).map(|e| e.docker_observe).unwrap_or(false);
    state
        .docker
        .remove_by_service_name(project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None)
        .await?;
    let proxy_injected = if docker_observe {
        plan.inject_docker_observe_proxy(project_id)?
    } else {
        false
    };
    if proxy_injected {
        state
            .docker
            .pull_image_with_opts(litebin_common::types::DOCKER_OBSERVE_PROXY_IMAGE, false)
            .await
            .map_err(|e| anyhow::anyhow!("failed to prepare Docker observation proxy: {e}"))?;
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
    if proxy_injected {
        let network = litebin_common::types::docker_observe_network_name(project_id, None);
        state.docker.ensure_named_network(&network).await?;
    }

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
    let mut configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();
    let healthy_wait_set: std::collections::HashSet<String> = plan
        .service_order
        .iter()
        .filter(|s| plan.needs_healthy_wait(s))
        .cloned()
        .collect();
    let completed_wait_set: std::collections::HashSet<String> = plan
        .service_order
        .iter()
        .filter(|s| plan.needs_completed_wait(s))
        .cloned()
        .collect();
    let has_healthcheck: std::collections::HashSet<String> = plan
        .service_order
        .iter()
        .filter(|s| {
            plan.configs
                .iter()
                .find(|c| c.service_name == **s)
                .and_then(|c| c.bollard_create_body.as_ref())
                .map(|body| body.healthcheck.is_some())
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    let mut public_container_id: Option<String> = None;
    let mut public_mapped_port: Option<u16> = None;
    let any_created = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let changed_container_ids = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    // Start services level by level — parallel within each level
    for level in &plan.service_levels {
        let mut tasks: JoinSet<Result<(String, u16, bool), String>> = JoinSet::new();

        for svc_name in level {
            let run_config = configs_map[svc_name].clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let is_public = run_config.is_public;
            let is_oneshot = run_config.is_oneshot;
            let is_host_network = run_config.host_network;
            let needs_healthy =
                healthy_wait_set.contains(svc_name) && has_healthcheck.contains(svc_name);
            let needs_completed = completed_wait_set.contains(svc_name) || is_oneshot;
            let pid = project_id.to_string();
            let any_created = any_created.clone();
            let changed_container_ids = changed_container_ids.clone();

            tasks.spawn(async move {
                let cname = litebin_common::types::container_name(&pid, &svc, None);

                // Check if container already exists and is running
                if let Ok(Some(existing_id)) = docker.find_container_by_name(&cname).await {
                    if docker.is_container_running(&existing_id).await.unwrap_or(false) {
                        tracing::info!(
                            project_id = %pid,
                            service = %svc,
                            "wake_multi_service: service already running, skipping"
                        );
                        let port = run_config.port.unwrap_or(80) as u16;
                        return Ok((existing_id, port, is_public));
                    }

                    // One-shot already exited successfully — leave it alone
                    if is_oneshot {
                        if matches!(
                            docker.container_exit_code(&existing_id).await.ok().flatten(),
                            Some(0)
                        ) {
                            tracing::info!(
                                project_id = %pid,
                                service = %svc,
                                "wake_multi_service: one-shot already completed, skipping"
                            );
                            let port = run_config.port.unwrap_or(80) as u16;
                            return Ok((existing_id, port, is_public));
                        }
                        // Failed or unknown exit — recreate
                        let _ = docker.remove_container(&existing_id).await;
                    } else {
                        // Long-running container exists but is stopped — start it
                        docker
                            .start_existing_container(&existing_id)
                            .await
                            .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;
                        if let Ok(mut ids) = changed_container_ids.lock() {
                            ids.push(existing_id.clone());
                        }
                        tracing::info!(
                            project_id = %pid,
                            service = %svc,
                            container = %existing_id,
                            "wake_multi_service: started existing stopped container"
                        );
                        let port = run_config.port.unwrap_or(80) as u16;
                        return Ok((existing_id, port, is_public));
                    }
                }

                let (container_id, mapped_port) = docker
                    .run_service_container(&run_config)
                    .await
                    .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;

                any_created.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut ids) = changed_container_ids.lock() {
                    ids.push(container_id.clone());
                }

                if svc == litebin_common::types::DOCKER_PROXY_SERVICE {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        let _ = docker.stop_container(&container_id).await;
                        let _ = docker.remove_container(&container_id).await;
                        return Err(format!("Docker observation proxy failed health check: {e}"));
                    }
                }

                if !is_host_network && docker.is_container_running(&container_id).await.unwrap_or(false) {
                    let _ = docker.wait_for_network_ready(&container_id).await;
                }

                if needs_healthy {
                    if let Err(e) = docker.wait_for_healthy(&container_id, true).await {
                        tracing::warn!(
                            project_id = %pid,
                            service = %svc,
                            error = %e,
                            "wake_multi_service: healthcheck failed, continuing"
                        );
                    }
                }

                if needs_completed {
                    docker
                        .wait_for_completed_successfully(&container_id)
                        .await
                        .map_err(|e| format!("one-shot service '{}' failed: {}", svc, e))?;
                }

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
                    if configs_map
                        .get(litebin_common::types::DOCKER_PROXY_SERVICE)
                        .is_some_and(|config| config.port == Some(2375))
                        && container_id
                            == changed_container_ids
                                .lock()
                                .ok()
                                .and_then(|ids| ids.last().cloned())
                                .unwrap_or_default()
                    {
                        let port = state
                            .docker
                            .inspect_mapped_port_for(&container_id, "2375/tcp")
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Docker observation proxy did not receive its required loopback mapping"))?;
                        for config in configs_map.values_mut() {
                            if config.host_network && config.docker_observe {
                                config.env.retain(|value| !value.starts_with("DOCKER_HOST="));
                                config.env.push(format!("DOCKER_HOST=tcp://127.0.0.1:{port}"));
                            }
                        }
                    }
                    if is_public {
                        public_container_id = Some(container_id.clone());
                        public_mapped_port = Some(mapped_port);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "wake_multi_service: failed to start service");
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    rollback_wake_containers(&state.docker, &changed_container_ids).await;
                    anyhow::bail!("{}", e);
                }
                Err(e) => {
                    tracing::error!(error = %e, "wake_multi_service: service task panicked");
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    rollback_wake_containers(&state.docker, &changed_container_ids).await;
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
