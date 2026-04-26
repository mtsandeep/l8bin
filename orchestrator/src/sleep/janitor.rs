use std::time::Duration;

use litebin_common::types::Node;

use crate::status::{self, ProjectUpdateFields};
use crate::AppState;

/// Background task that periodically stops idle containers.
pub async fn run_janitor(state: AppState) {
    let interval = Duration::from_secs(state.config.janitor_interval_secs);

    tracing::info!(
        interval_secs = state.config.janitor_interval_secs,
        "janitor started"
    );

    loop {
        tokio::time::sleep(interval).await;

        let r = state.router.read().await.clone();
        if let Err(e) = sweep(&state, r.as_ref()).await {
            tracing::error!(error = %e, "janitor sweep failed");
        }
    }
}

async fn sweep(
    state: &AppState,
    router: &dyn litebin_common::routing::RoutingProvider,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();

    let candidates = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE status = 'running' AND auto_stop_enabled = 1",
    )
    .fetch_all(&state.db)
    .await?;

    let idle_projects: Vec<crate::db::models::Project> = candidates
        .into_iter()
        .filter(|p| {
            let timeout_secs = p.auto_stop_timeout_mins * 60;
            p.last_active_at.map(|t| now - t >= timeout_secs).unwrap_or(true)
        })
        .collect();

    if idle_projects.is_empty() {
        tracing::debug!("janitor: no idle projects");
        return Ok(());
    }

    tracing::info!(count = idle_projects.len(), "janitor: found idle projects");

    // Pre-load service containers BEFORE marking as stopped (transition cascades
    // service status, so we need the container_ids while services still show 'running').
    let mut service_containers: std::collections::HashMap<String, Vec<(String, Option<String>)>> = std::collections::HashMap::new();
    for project in &idle_projects {
        let rows: Vec<(String, Option<String>)> = if project.service_count.unwrap_or(1) > 1 {
            sqlx::query_as(
                "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL AND container_id != ''",
            )
            .bind(&project.id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
        } else {
            // Single-service: use projects.container_id
            project.container_id.as_ref().map(|cid| vec![("web".to_string(), Some(cid.clone()))]).unwrap_or_default()
        };
        if !rows.is_empty() {
            service_containers.insert(project.id.clone(), rows);
        }
    }

    // 1. Mark all idle projects as stopped in DB first
    for project in &idle_projects {
        status::transition(
            &state.db,
            &project.id,
            "stopped",
            &ProjectUpdateFields {
                mapped_port: Some(None),
                ..Default::default()
            },
            None,
        )
        .await?;
    }

    // 2. Resync routes — stopped projects are now excluded, so requests hit the
    //    waker instead of a dead upstream (eliminates the 502/timeout window)
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let routes = crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await?;

    router
        .sync_routes(&routes, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, false)
        .await?;

    // 3. Now safe to stop containers — routes already removed
    // Use pre-loaded container_ids (transition already set services to 'stopped',
    // so querying status='running' would return nothing).
    for project in &idle_projects {
        let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);
        let containers = service_containers.get(&project.id).cloned().unwrap_or_default();

        if project.service_count.unwrap_or(1) > 1 {
            // Multi-service: stop all service containers
            if is_local {
                for (_svc_name, cid) in containers.iter().rev() {
                    if let Some(container_id) = cid {
                        stop_local_container(state, &project.id, container_id).await;
                    }
                }
            } else {
                let node_id = match project.node_id.as_deref() {
                    Some(n) if n != "local" => n.to_string(),
                    _ => continue,
                };
                for (_svc_name, cid) in containers.iter().rev() {
                    if let Some(container_id) = cid {
                        stop_remote_container(state, &project.id, &node_id, container_id).await;
                    }
                }
            }
        } else if let Some((_svc_name, cid)) = containers.first() {
            // Single-service: stop the one container
            if let Some(container_id) = cid {
                if is_local {
                    stop_local_container(state, &project.id, container_id).await;
                } else {
                    let node_id = project.node_id.as_deref().unwrap();
                    stop_remote_container(state, &project.id, node_id, container_id).await;
                }
            }
        }
    }

    Ok(())
}

async fn stop_local_container(state: &AppState, project_id: &str, container_id: &str) {
    if let Err(e) = state.docker.stop_container(container_id).await {
        let err_str = e.to_string();
        if err_str.contains("404") || err_str.contains("No such container") {
            tracing::warn!(project = %project_id, "janitor: container gone, already stopped");
        } else {
            tracing::error!(project = %project_id, error = %e, "janitor: failed to stop container (orphan — cleaned up on next wake)");
        }
    } else {
        tracing::info!(project = %project_id, "janitor: container stopped (idle)");
    }
}

async fn stop_remote_container(state: &AppState, project_id: &str, node_id: &str, container_id: &str) {
    let client = match crate::nodes::client::get_node_client(&state.node_clients, node_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(project = %project_id, error = %e, "janitor: node client unavailable");
            return;
        }
    };

    let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(n)) => n,
        _ => {
            tracing::error!(project = %project_id, "janitor: node not found");
            return;
        }
    };

    let base_url = crate::routes::manage::agent_base_url(&state.config, &node);

    match client
        .post(&format!("{}/containers/stop", base_url))
        .json(&serde_json::json!({"container_id": container_id}))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(project = %project_id, "janitor: remote container stopped (idle)");
        }
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(project = %project_id, body = %body, "janitor: remote stop returned non-success");
        }
        Err(e) => {
            tracing::error!(project = %project_id, error = %e, "janitor: failed to stop remote container");
        }
    }
}
