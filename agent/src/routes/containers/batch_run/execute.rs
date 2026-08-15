use axum::http::StatusCode;
use tokio::task::JoinSet;

use crate::AgentState;

use super::rollback::{batch_run_error, rollback_started_containers, wait_for_proxy_ready};
use super::types::{BatchRunRequest, ServiceRunResult};

/// Start services level by level — parallel within each level. Rolls back
/// everything on any failure (including proxy readiness) and reports the
/// removed-services metadata.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_levels(
    state: &AgentState,
    req: &BatchRunRequest,
    plan: &litebin_common::compose_run::ComposeRunPlan,
    target_set: &Option<std::collections::HashSet<String>>,
    removed_services: &[String],
    configs_map: &mut std::collections::HashMap<String, litebin_common::types::RunServiceConfig>,
) -> Result<Vec<ServiceRunResult>, axum::response::Response> {
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
            if let Some(targets) = target_set
                && !targets.contains(svc_name)
            {
                continue;
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
                            rollback_started_containers(
                                &state.docker,
                                &req.project_id,
                                &operation_services,
                                &started_container_ids,
                            )
                            .await;
                            return Err(batch_run_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Docker observation proxy failed to start",
                                removed_services,
                            ));
                        };
                        if let Err(e) = wait_for_proxy_ready(&state.docker, cid).await {
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            let _ = state.docker.stop_container(cid).await;
                            let _ = state.docker.remove_container(cid).await;
                            rollback_started_containers(
                                &state.docker,
                                &req.project_id,
                                &operation_services,
                                &started_container_ids,
                            )
                            .await;
                            return Err(batch_run_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Docker observation proxy failed health check: {e}"),
                                removed_services,
                            ));
                        }
                        if configs_map.values().any(|config| config.host_network && config.docker_observe) {
                            let port = match state.docker.inspect_mapped_port_for(cid, "2375/tcp").await {
                                Ok(Some(port)) => port,
                                Ok(None) => {
                                    rollback_started_containers(
                                        &state.docker,
                                        &req.project_id,
                                        &operation_services,
                                        &started_container_ids,
                                    )
                                    .await;
                                    return Err(batch_run_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "Docker observation proxy did not receive its required loopback mapping",
                                        removed_services,
                                    ));
                                }
                                Err(error) => {
                                    rollback_started_containers(
                                        &state.docker,
                                        &req.project_id,
                                        &operation_services,
                                        &started_container_ids,
                                    )
                                    .await;
                                    return Err(batch_run_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("failed to inspect Docker observation proxy mapping: {error}"),
                                        removed_services,
                                    ));
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
                    rollback_started_containers(
                        &state.docker,
                        &req.project_id,
                        &operation_services,
                        &started_container_ids,
                    )
                    .await;
                    return Err(batch_run_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("service task failed: {e}"),
                        removed_services,
                    ));
                }
            }
        }
        let level_errors: Vec<String> = results
            .iter()
            .filter_map(|result| result.error.as_ref().map(|error| format!("{}: {}", result.service_name, error)))
            .collect();
        if !level_errors.is_empty() {
            rollback_started_containers(&state.docker, &req.project_id, &operation_services, &started_container_ids)
                .await;
            return Err(batch_run_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("one or more services failed: {}", level_errors.join("; ")),
                removed_services,
            ));
        }
    }

    Ok(results)
}
