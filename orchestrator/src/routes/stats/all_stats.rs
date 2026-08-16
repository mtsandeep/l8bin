use axum::{Json, extract::State, http::StatusCode};

use super::super::manage::{get_node_from_db, sync_caddy};
use super::helpers::{
    batch_load_services, compose_service_ports, enrich_services, inactive_project_status, make_stats_response,
    project_container_ids,
};
use super::types::{BatchStatsResponse, LiveStats, ServiceInfo, StatsResponse};
use crate::AppState;
use crate::nodes;
use crate::status;
use litebin_common::types::ProjectStatus;

/// GET /projects/stats — returns stats + disk + services for all projects in one call
#[utoipa::path(
    get,
    path = "/projects/stats",
    responses(
        (status = 200, body = BatchStatsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn all_project_stats(
    State(state): State<AppState>,
) -> Result<Json<BatchStatsResponse>, (StatusCode, String)> {
    let t_total = std::time::Instant::now();

    // Periodic background sync (60s) handles Docker reconciliation.
    // Stats endpoint just reads DB — no need to sync on every poll.
    let mut caddy_dirty = false;

    let projects = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects")
        .fetch_all(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Batch-load services for ALL projects upfront
    let all_ids: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let mut services_map = batch_load_services(&state.db, &all_ids).await;

    // Attach every container port per service (compose projects) for route suggestions.
    let compose_ports = compose_service_ports(&projects);
    for (pid, services) in services_map.iter_mut() {
        if let Some(svc_ports) = compose_ports.get(pid) {
            for (svc, _) in services.iter_mut() {
                if let Some(ports) = svc_ports.get(&svc.service_name) {
                    svc.ports = ports.clone();
                }
            }
        }
    }

    // Map project_id -> last_active_at for stats response
    let last_active_map: std::collections::HashMap<String, Option<i64>> =
        projects.iter().map(|p| (p.id.clone(), p.last_active_at)).collect();

    let mut results: Vec<StatsResponse> = Vec::with_capacity(projects.len());

    // For local projects: (project_id, Vec<container_id>)
    let mut local_projects: Vec<(String, Vec<String>)> = Vec::new();
    // For remote projects: node_id -> Vec<(project_id, Vec<container_id>)>
    let mut remote_by_node: std::collections::HashMap<String, Vec<(String, Vec<String>)>> =
        std::collections::HashMap::new();
    // Stopped local projects that need per-service disk lookups: (project_id, services_raw)
    let mut disk_lookups: Vec<(String, ServicesRaw)> = Vec::new();

    for project in &projects {
        if project.status != ProjectStatus::Running && project.status != ProjectStatus::Degraded {
            let services_raw = services_map.get(&project.id).cloned().unwrap_or_default();

            // Check if any service has a container_id for disk lookup
            let has_any_cid = services_raw.iter().any(|(_, cid)| cid.is_some());
            if !has_any_cid {
                results.push(make_stats_response(
                    project.id.clone(),
                    project.status.clone(),
                    project.last_active_at,
                    services_raw.into_iter().map(|(s, _)| s).collect(),
                ));
                continue;
            }

            let node_id = project.node_id.as_deref().unwrap_or("local");
            if node_id == "local" {
                disk_lookups.push((project.id.clone(), services_raw));
            } else {
                results.push(make_stats_response(
                    project.id.clone(),
                    project.status.clone(),
                    project.last_active_at,
                    services_raw.into_iter().map(|(s, _)| s).collect(),
                ));
            }
            continue;
        }

        // Running project — get all container IDs (multi-service aware)
        let (container_ids, _is_multi) = project_container_ids(&state.db, project).await;
        let services_raw = services_map.get(&project.id).cloned().unwrap_or_default();

        if container_ids.is_empty() {
            results.push(make_stats_response(
                project.id.clone(),
                project.status.clone(),
                project.last_active_at,
                services_raw.into_iter().map(|(s, _)| s).collect(),
            ));
            continue;
        }

        let node_id = project.node_id.as_deref().unwrap_or("local");
        if node_id == "local" {
            local_projects.push((project.id.clone(), container_ids));
        } else {
            remote_by_node.entry(node_id.to_string()).or_default().push((project.id.clone(), container_ids));
        }
    }

    // Handle per-service disk lookups for stopped local containers
    let stopped_count = disk_lookups.len();
    for (project_id, services_raw) in disk_lookups {
        let mut services: Vec<ServiceInfo> = Vec::with_capacity(services_raw.len());
        for (mut svc, cid) in services_raw {
            if let Some(ref container_id) = cid {
                // Use cached disk value for stopped containers — disk doesn't change
                // when container isn't running. Only call Docker if we have no cached value.
                if let Some(bytes) = state.disk_cache.get(container_id) {
                    svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                } else if let Ok(d) = state.docker.disk_usage(container_id).await {
                    // Container gone, no cache to fall back to — leave disk unset on error
                    let disk_gb = d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0);
                    svc.disk_gb = Some(disk_gb);
                    state.disk_cache.insert(container_id.clone(), d.size_root_fs as i64);
                }
            }
            services.push(svc);
        }
        // Use the actual project status — preserve transient/setup states
        // and only derive stopped/degraded when the project is in a stable terminal state
        let project_status =
            projects.iter().find(|p| p.id == project_id).map(|p| p.status.clone()).unwrap_or(ProjectStatus::Stopped);
        let status = inactive_project_status(project_status, &services);
        results.push(make_stats_response(
            project_id.clone(),
            status,
            last_active_map.get(&project_id).copied().flatten(),
            services,
        ));
    }

    // Fetch local stats — parallelize all Docker API calls across all containers
    let t2 = std::time::Instant::now();

    /// Per-service row with its container_id, as loaded by batch_load_services.
    type ServicesRaw = Vec<(ServiceInfo, Option<String>)>;
    /// (container_id, live cpu/mem triple) — None when stats collection failed.
    type ContainerStatsEntry = (String, Option<(f64, u64, u64)>);

    // Flatten all container IDs with their project context
    let mut all_local_containers: Vec<(String, String)> = Vec::new(); // (project_id, container_id)
    for (project_id, container_ids) in &local_projects {
        for cid in container_ids {
            all_local_containers.push((project_id.clone(), cid.clone()));
        }
    }

    // Parallel Docker calls for all containers at once
    let mut handles = Vec::with_capacity(all_local_containers.len());
    for (_project_id, cid) in &all_local_containers {
        let docker = state.docker.clone();
        let cid = cid.clone();
        handles.push(async move {
            let stats_res = docker.container_stats(&cid).await;
            (cid, stats_res)
        });
    }
    let mut container_results: Vec<ContainerStatsEntry> = Vec::with_capacity(handles.len());
    for handle in handles {
        let (cid, stats_res) = handle.await;
        match stats_res {
            Ok(s) => container_results.push((cid, Some((s.cpu_percent, s.memory_usage, s.memory_limit)))),
            Err(_) => container_results.push((cid, None)),
        }
    }

    // Build per-project results from container data
    let container_running: std::collections::HashSet<String> =
        container_results.iter().filter(|(_, r)| r.is_some()).map(|(cid, _)| cid.clone()).collect();

    for (project_id, container_ids) in &local_projects {
        let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
        let mut per_container: std::collections::HashMap<String, LiveStats> = std::collections::HashMap::new();
        let mut stopped_cids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut any_running = false;

        for cid in container_ids {
            if container_running.contains(cid) {
                any_running = true;
                // Disk cache: use cached value or fetch in background
                let (disk, cpu_limit) = if let Some(bytes) = state.disk_cache.get(cid) {
                    (*bytes as f64 / (1024.0 * 1024.0 * 1024.0), None)
                } else {
                    // No cached disk — fetch (only on first encounter per container)
                    match state.docker.disk_usage(cid).await {
                        Ok(d) => {
                            state.disk_cache.insert(cid.clone(), d.size_root_fs as i64);
                            (d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0), d.cpu_limit)
                        }
                        Err(_) => (0.0, None),
                    }
                };
                // Find matching stats result
                let (cpu, mem_usage, mem_limit) =
                    container_results.iter().find(|(c, _)| c == cid).and_then(|(_, r)| *r).unwrap_or((0.0, 0, 0));
                per_container.insert(cid.clone(), (cpu, mem_usage, mem_limit, disk, cpu_limit));
            } else {
                stopped_cids.insert(cid.clone());
                // Cache disk for stopped containers
                if !state.disk_cache.contains_key(cid)
                    && let Ok(d) = state.docker.disk_usage(cid).await
                {
                    state.disk_cache.insert(cid.clone(), d.size_root_fs as i64);
                }
            }
        }

        if !any_running {
            let services: Vec<ServiceInfo> = services_raw
                .into_iter()
                .map(|(mut svc, cid)| {
                    if let Some(container_id) = cid
                        && let Some(bytes) = state.disk_cache.get(&container_id)
                    {
                        svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                    }
                    svc
                })
                .collect();
            results.push(make_stats_response(
                project_id.clone(),
                ProjectStatus::Stopped,
                last_active_map.get(project_id).copied().flatten(),
                services,
            ));
            continue;
        }

        let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
        results.push(make_stats_response(
            project_id.clone(),
            ProjectStatus::Running,
            last_active_map.get(project_id).copied().flatten(),
            services,
        ));
    }

    tracing::info!(elapsed_ms = t2.elapsed().as_millis(), "stats: local docker stats");

    // Fetch remote stats — one POST per node with all container IDs
    let t3 = std::time::Instant::now();
    for (node_id, projects_containers) in &remote_by_node {
        // Flatten all container IDs for the batch request
        let all_container_ids: Vec<String> =
            projects_containers.iter().flat_map(|(_, cids)| cids.iter().cloned()).collect();

        // Map container_id -> project_id
        let cid_to_pid: std::collections::HashMap<String, String> = projects_containers
            .iter()
            .flat_map(|(pid, cids)| cids.iter().map(move |cid| (cid.clone(), pid.clone())))
            .collect();

        let client = match nodes::client::get_node_client(&state.node_clients, node_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: node client unavailable");
                for (project_id, _) in projects_containers {
                    let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                    results.push(make_stats_response(
                        project_id.clone(),
                        ProjectStatus::Running,
                        last_active_map.get(project_id).copied().flatten(),
                        services_raw.into_iter().map(|(s, _)| s).collect(),
                    ));
                }
                continue;
            }
        };

        let node = match get_node_from_db(&state.db, node_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = ?e, "batch stats: node not found");
                for (project_id, _) in projects_containers {
                    let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                    results.push(make_stats_response(
                        project_id.clone(),
                        ProjectStatus::Running,
                        last_active_map.get(project_id).copied().flatten(),
                        services_raw.into_iter().map(|(s, _)| s).collect(),
                    ));
                }
                continue;
            }
        };

        let agent = nodes::client::AgentClient::new(client, &node, &state.config);

        let items =
            match agent.stats(&litebin_common::agent_api::BatchStatsRequest { container_ids: all_container_ids }).await
            {
                Ok(items) => items,
                Err(nodes::client::AgentClientError::Parse(e)) => {
                    // Unparseable body: keep going with no live samples — projects on
                    // this node fall back to Stopped below.
                    tracing::warn!(node_id = %node_id, error = %e, "batch stats: failed to parse response");
                    Vec::new()
                }
                Err(nodes::client::AgentClientError::Status { body, .. }) => {
                    tracing::warn!(node_id = %node_id, body = %body, "batch stats: agent returned error");
                    for (project_id, _) in projects_containers {
                        let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                        results.push(make_stats_response(
                            project_id.clone(),
                            ProjectStatus::Running,
                            last_active_map.get(project_id).copied().flatten(),
                            services_raw.into_iter().map(|(s, _)| s).collect(),
                        ));
                    }
                    continue;
                }
                Err(e) => {
                    tracing::warn!(node_id = %node_id, error = %e, "batch stats: agent unreachable");
                    for (project_id, _) in projects_containers {
                        let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                        results.push(make_stats_response(
                            project_id.clone(),
                            ProjectStatus::Running,
                            last_active_map.get(project_id).copied().flatten(),
                            services_raw.into_iter().map(|(s, _)| s).collect(),
                        ));
                    }
                    continue;
                }
            };

        // Collect per-container stats and group by project
        let mut per_container: std::collections::HashMap<String, LiveStats> = std::collections::HashMap::new();
        let mut project_stats: std::collections::HashMap<String, (f64, u64, u64, f64)> =
            std::collections::HashMap::new();
        let mut stopped_cids: std::collections::HashSet<String> = std::collections::HashSet::new();

        {
            // Collect container states per project for DB sync
            let mut container_states_by_project: std::collections::HashMap<String, Vec<(String, bool)>> =
                std::collections::HashMap::new();

            for item in &items {
                let cid = item.container_id.as_str();
                let project_id = cid_to_pid.get(cid).cloned().unwrap_or_default();
                let is_running = item.state != "stopped";
                let disk_gb = item.disk_gb;

                container_states_by_project.entry(project_id.clone()).or_default().push((cid.to_string(), is_running));

                if disk_gb > 0.0 {
                    state.disk_cache.insert(cid.to_string(), (disk_gb * 1024.0 * 1024.0 * 1024.0) as i64);
                }

                if !is_running {
                    stopped_cids.insert(cid.to_string());
                    caddy_dirty = true;
                    continue;
                }

                let cpu = item.cpu_percent;
                let mem_usage = item.memory_usage;
                let mem_limit = item.memory_limit;
                let cpu_limit = item.cpu_limit;

                per_container.insert(cid.to_string(), (cpu, mem_usage, mem_limit, disk_gb, cpu_limit));

                let entry = project_stats.entry(project_id.clone()).or_insert((0.0, 0, 0, 0.0));
                entry.0 += cpu;
                entry.1 += mem_usage;
                entry.2 += mem_limit;
                entry.3 += disk_gb;
            }

            // Sync remote project statuses from agent-reported container states
            for (pid, states) in &container_states_by_project {
                let result = status::update_status_from_container_states(&state.db, pid, states).await;
                if result.caddy_dirty {
                    caddy_dirty = true;
                }
            }
        }

        // Build results for each project on this node
        for (project_id, _) in projects_containers {
            let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
            if project_stats.contains_key(project_id) {
                let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
                results.push(make_stats_response(
                    project_id.clone(),
                    ProjectStatus::Running,
                    last_active_map.get(project_id).copied().flatten(),
                    services,
                ));
            } else {
                let services: Vec<ServiceInfo> = services_raw
                    .into_iter()
                    .map(|(mut svc, cid)| {
                        if let Some(container_id) = cid
                            && let Some(bytes) = state.disk_cache.get(&container_id)
                        {
                            svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                        }
                        svc
                    })
                    .collect();
                results.push(make_stats_response(
                    project_id.clone(),
                    ProjectStatus::Stopped,
                    last_active_map.get(project_id).copied().flatten(),
                    services,
                ));
            }
        }
    }
    tracing::info!(elapsed_ms = t3.elapsed().as_millis(), "stats: remote node stats");

    if caddy_dirty {
        sync_caddy(&state).await;
    }

    tracing::info!(
        elapsed_ms = t_total.elapsed().as_millis(),
        projects = results.len(),
        local = local_projects.len(),
        remote = remote_by_node.len(),
        stopped = stopped_count,
        "stats: total"
    );

    Ok(Json(BatchStatsResponse { stats: results }))
}
