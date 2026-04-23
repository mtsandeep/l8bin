use axum::{extract::Path, extract::Query, extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::nodes;
use crate::AppState;
use super::manage::{agent_base_url, get_node_from_db, sync_caddy};

#[derive(Serialize, Clone)]
pub struct ServiceInfo {
    pub service_name: String,
    pub image: String,
    pub port: Option<i64>,
    pub is_public: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_usage: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_gb: Option<f64>,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceInfo>,
}

#[derive(Serialize)]
pub struct BatchStatsResponse {
    pub stats: Vec<StatsResponse>,
}

#[derive(Serialize)]
pub struct DiskUsageResponse {
    pub project_id: String,
    pub size_gb: f64,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub tail: Option<usize>,
    pub service: Option<String>,
}

#[derive(Serialize)]
pub struct LogsResponse {
    pub project_id: String,
    pub service_name: Option<String>,
    pub lines: Vec<String>,
}

/// Per-container live stats collected from Docker.
/// (cpu_percent, memory_usage, memory_limit, disk_gb, cpu_limit)
type LiveStats = (f64, u64, u64, f64, Option<f64>);

/// Given a map of container_id -> live stats,
/// attach per-service stats to each ServiceInfo and return the final Vec.
/// When running, memory_limit from Docker stats overrides the DB value.
/// cpu_limit from inspect is used if DB value is None.
/// Container IDs in `stopped_cids` are marked as "stopped" with disk from cache.
fn enrich_services(
    services: &[(ServiceInfo, Option<String>)],
    stats_map: &std::collections::HashMap<String, LiveStats>,
    stopped_cids: &std::collections::HashSet<String>,
    disk_cache: &dashmap::DashMap<String, i64>,
) -> Vec<ServiceInfo> {
    services.iter().map(|(svc, cid)| {
        let mut enriched = svc.clone();
        if let Some(container_id) = cid {
            if stopped_cids.contains(container_id) {
                enriched.status = "stopped".to_string();
                enriched.cpu_percent = None;
                enriched.memory_usage = None;
                if let Some(bytes) = disk_cache.get(container_id) {
                    enriched.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                }
                return enriched;
            }
            if let Some(&(cpu, mem_usage, mem_limit, disk, cpu_limit)) = stats_map.get(container_id) {
                enriched.cpu_percent = Some(cpu);
                enriched.memory_usage = Some(mem_usage);
                enriched.memory_limit = Some(mem_limit);
                enriched.disk_gb = Some(disk);
                if enriched.cpu_limit.is_none() {
                    enriched.cpu_limit = cpu_limit;
                }
            }
        }
        enriched
    }).collect()
}

/// Collect all container IDs for a project (single or multi-service).
/// Returns (container_ids, is_multi_service).
async fn project_container_ids(
    db: &sqlx::SqlitePool,
    project: &crate::db::models::Project,
) -> (Vec<String>, bool) {
    if project.service_count.unwrap_or(1) > 1 {
        let services: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        let ids: Vec<String> = services.into_iter()
            .filter_map(|(cid,)| cid)
            .collect();
        (ids, true)
    } else {
        let ids = project.container_id.clone()
            .map(|cid| vec![cid])
            .unwrap_or_default();
        (ids, false)
    }
}

/// Batch-load services for all given project IDs.
/// Returns a map: project_id -> Vec<(ServiceInfo, Option<container_id>)>.
/// The container_id is used to look up per-service stats.
async fn batch_load_services(
    db: &sqlx::SqlitePool,
    project_ids: &[String],
) -> std::collections::HashMap<String, Vec<(ServiceInfo, Option<String>)>> {
    if project_ids.is_empty() {
        return std::collections::HashMap::new();
    }

    // Load from project_services table
    let placeholders = project_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT project_id, service_name, image, port, is_public, status, container_id, memory_limit_mb, cpu_limit FROM project_services WHERE project_id IN ({}) ORDER BY service_name",
        placeholders
    );

    let mut builder = sqlx::query_as::<_, (String, String, String, Option<i64>, bool, String, Option<String>, Option<i64>, Option<f64>)>(&query);
    for pid in project_ids {
        builder = builder.bind(pid);
    }

    let rows = builder.fetch_all(db).await.unwrap_or_default();

    // Group by project_id
    let mut map: std::collections::HashMap<String, Vec<(ServiceInfo, Option<String>)>> = std::collections::HashMap::new();
    for (project_id, service_name, image, port, is_public, status, container_id, memory_limit_mb, cpu_limit) in rows {
        let memory_limit = memory_limit_mb.map(|mb| (mb as u64) * 1024 * 1024);
        map.entry(project_id).or_default().push((
            ServiceInfo {
                service_name,
                image,
                port,
                is_public,
                status,
                cpu_percent: None,
                memory_usage: None,
                memory_limit,
                cpu_limit,
                disk_gb: None,
            },
            container_id,
        ));
    }

    // For single-service projects that have no project_services row, synthesize one from the projects table
    let ids_with_services: std::collections::HashSet<String> = map.keys().cloned().collect();
    for pid in project_ids {
        if ids_with_services.contains(pid) {
            continue;
        }
        // Check if this project has an image (deployed)
        let row: Option<(String, Option<i64>, String, Option<String>)> = sqlx::query_as(
            "SELECT image, internal_port, status, container_id FROM projects WHERE id = ?"
        )
        .bind(pid)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

        if let Some((image, port, status, container_id)) = row {
            if !image.is_empty() {
                // Fetch limits from projects table for single-service fallback
                let limits: Option<(Option<i64>, Option<f64>)> = sqlx::query_as(
                    "SELECT memory_limit_mb, cpu_limit FROM projects WHERE id = ?"
                )
                .bind(pid)
                .fetch_optional(db)
                .await
                .unwrap_or(None)
                .and_then(|r: (Option<i64>, Option<f64>)| Some(r));

                let (memory_limit, cpu_limit) = limits
                    .map(|(mb, cpu)| (mb.map(|m| (m as u64) * 1024 * 1024), cpu))
                    .unwrap_or((None, None));

                map.entry(pid.clone()).or_default().push((
                    ServiceInfo {
                        service_name: "web".to_string(),
                        image,
                        port,
                        is_public: true,
                        status,
                        cpu_percent: None,
                        memory_usage: None,
                        memory_limit,
                        cpu_limit,
                        disk_gb: None,
                    },
                    container_id,
                ));
            }
        }
    }

    map
}

fn make_stats_response(project_id: String, status: String, services: Vec<ServiceInfo>) -> StatsResponse {
    // Compute effective status: if DB says "running" but not all services are up, mark as "degraded"
    let effective_status = if status == "running"
        && !services.is_empty()
        && services.iter().any(|s| s.status != "running")
    {
        "degraded".to_string()
    } else {
        status
    };
    StatsResponse { project_id, status: effective_status, services }
}

/// GET /projects/stats — returns stats + disk + services for all projects in one call
pub async fn all_project_stats(
    State(state): State<AppState>,
) -> Result<Json<BatchStatsResponse>, (StatusCode, String)> {
    let projects = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Batch-load services for ALL projects upfront
    let all_ids: Vec<String> = projects.iter().map(|p| p.id.clone()).collect();
    let services_map = batch_load_services(&state.db, &all_ids).await;

    let mut results: Vec<StatsResponse> = Vec::with_capacity(projects.len());
    let mut caddy_dirty = false;

    // For local projects: (project_id, Vec<container_id>)
    let mut local_projects: Vec<(String, Vec<String>)> = Vec::new();
    // For remote projects: node_id -> Vec<(project_id, Vec<container_id>)>
    let mut remote_by_node: std::collections::HashMap<String, Vec<(String, Vec<String>)>> = std::collections::HashMap::new();
    // Stopped local projects that need per-service disk lookups: (project_id, services_raw)
    let mut disk_lookups: Vec<(String, Vec<(ServiceInfo, Option<String>)>)> = Vec::new();

    for project in &projects {
        if project.status != "running" {
            let services_raw = services_map.get(&project.id).cloned().unwrap_or_default();

            // Check if any service has a container_id for disk lookup
            let has_any_cid = services_raw.iter().any(|(_, cid)| cid.is_some());
            if !has_any_cid {
                results.push(make_stats_response(
                    project.id.clone(),
                    project.status.clone(),
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
                services_raw.into_iter().map(|(s, _)| s).collect(),
            ));
            continue;
        }

        let node_id = project.node_id.as_deref().unwrap_or("local");
        if node_id == "local" {
            local_projects.push((project.id.clone(), container_ids));
        } else {
            remote_by_node
                .entry(node_id.to_string())
                .or_default()
                .push((project.id.clone(), container_ids));
        }
    }

    // Handle per-service disk lookups for stopped local containers
    for (project_id, services_raw) in disk_lookups {
        let mut services: Vec<ServiceInfo> = Vec::with_capacity(services_raw.len());
        for (mut svc, cid) in services_raw {
            if let Some(ref container_id) = cid {
                match state.docker.disk_usage(container_id).await {
                    Ok(d) => {
                        let disk_gb = d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0);
                        svc.disk_gb = Some(disk_gb);
                        state.disk_cache.insert(container_id.clone(), d.size_root_fs as i64);
                    }
                    Err(_) => {
                        // Container removed or unreachable — fall back to cached value
                        if let Some(bytes) = state.disk_cache.get(container_id) {
                            svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                        }
                    }
                }
            }
            services.push(svc);
        }
        results.push(make_stats_response(
            project_id,
            "stopped".to_string(),
            services,
        ));
    }

    // Fetch local stats — per-container for per-service breakdown
    for (project_id, container_ids) in &local_projects {
        let services_raw = services_map.get(project_id).cloned().unwrap_or_default();

        // Collect stats for each container
        let mut per_container: std::collections::HashMap<String, LiveStats> = std::collections::HashMap::new();
        let mut any_running = false;
        let mut stopped_cids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for cid in container_ids {
            let actually_running = state.docker.is_container_running(cid).await.unwrap_or(false);
            if !actually_running {
                stopped_cids.insert(cid.clone());
                // Try to cache disk before marking stopped
                if let Ok(d) = state.docker.disk_usage(cid).await {
                    state.disk_cache.insert(cid.clone(), d.size_root_fs as i64);
                }
                // Update this specific service to stopped in DB
                let _ = sqlx::query("UPDATE project_services SET status = 'stopped' WHERE project_id = ? AND container_id = ?")
                    .bind(project_id)
                    .bind(cid)
                    .execute(&state.db)
                    .await;
                caddy_dirty = true;
                continue;
            }
            any_running = true;

            let stats_fut = state.docker.container_stats(cid);
            let disk_fut = state.docker.disk_usage(cid);
            let (stats_res, disk_res) = tokio::join!(stats_fut, disk_fut);

            let (cpu, mem_usage, mem_limit) = match stats_res {
                Ok(s) => (s.cpu_percent, s.memory_usage, s.memory_limit),
                Err(e) => {
                    tracing::warn!(project = %project_id, container = %cid, error = %e, "batch stats: failed to fetch local stats");
                    (0.0, 0, 0)
                }
            };

            let (disk, cpu_limit) = match disk_res {
                Ok(d) => {
                    state.disk_cache.insert(cid.clone(), d.size_root_fs as i64);
                    (d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0), d.cpu_limit)
                }
                Err(_) => (0.0, None),
            };

            per_container.insert(cid.clone(), (cpu, mem_usage, mem_limit, disk, cpu_limit));
        }

        if !any_running {
            // All containers stopped — mark project as stopped
            let now = chrono::Utc::now().timestamp();
            let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(project_id)
                .execute(&state.db)
                .await;
            let services: Vec<ServiceInfo> = services_raw.into_iter().map(|(mut svc, cid)| {
                if let Some(container_id) = cid {
                    if let Some(bytes) = state.disk_cache.get(&container_id) {
                        svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                    }
                }
                svc
            }).collect();
            results.push(make_stats_response(
                project_id.clone(),
                "stopped".to_string(),
                services,
            ));
            continue;
        }

        let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
        results.push(make_stats_response(
            project_id.clone(),
            "running".to_string(),
            services,
        ));

        // If some services are stopped but not all, mark project as "degraded" in DB
        // so the route resolver points traffic to the orchestrator waker
        if !stopped_cids.is_empty() && any_running {
            let now = chrono::Utc::now().timestamp();
            let _ = sqlx::query("UPDATE projects SET status = 'degraded', updated_at = ? WHERE id = ? AND status != 'degraded'")
                .bind(now)
                .bind(project_id)
                .execute(&state.db)
                .await;
            if !caddy_dirty {
                caddy_dirty = true;
            }
        }
    }

    // Fetch remote stats — one POST per node with all container IDs
    for (node_id, projects_containers) in &remote_by_node {
        // Flatten all container IDs for the batch request
        let all_container_ids: Vec<String> = projects_containers.iter()
            .flat_map(|(_, cids)| cids.iter().cloned())
            .collect();

        // Map container_id -> project_id
        let cid_to_pid: std::collections::HashMap<String, String> = projects_containers.iter()
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
                        "running".to_string(),
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
                        "running".to_string(),
                        services_raw.into_iter().map(|(s, _)| s).collect(),
                    ));
                }
                continue;
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        let resp = match client
            .post(format!("{}/containers/stats", base_url))
            .json(&json!({ "container_ids": all_container_ids }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: agent unreachable");
                for (project_id, _) in projects_containers {
                    let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                    results.push(make_stats_response(
                        project_id.clone(),
                        "running".to_string(),
                        services_raw.into_iter().map(|(s, _)| s).collect(),
                    ));
                }
                continue;
            }
        };

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(node_id = %node_id, body = %body, "batch stats: agent returned error");
            for (project_id, _) in projects_containers {
                let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
                results.push(make_stats_response(
                    project_id.clone(),
                    "running".to_string(),
                    services_raw.into_iter().map(|(s, _)| s).collect(),
                ));
            }
            continue;
        }

        // Collect per-container stats and group by project
        let mut per_container: std::collections::HashMap<String, LiveStats> = std::collections::HashMap::new();
        let mut project_stats: std::collections::HashMap<String, (f64, u64, u64, f64)> = std::collections::HashMap::new();
        let mut stopped_cids: std::collections::HashSet<String> = std::collections::HashSet::new();

        match resp.json::<Vec<serde_json::Value>>().await {
            Ok(items) => {
                for item in &items {
                    let cid = item["container_id"].as_str().unwrap_or("");
                    let project_id = cid_to_pid.get(cid).cloned().unwrap_or_default();
                    let state_str = item["state"].as_str().unwrap_or("running");
                    let disk_gb = item["disk_gb"].as_f64().unwrap_or(0.0);

                    if disk_gb > 0.0 {
                        state.disk_cache.insert(cid.to_string(), (disk_gb * 1024.0 * 1024.0 * 1024.0) as i64);
                    }

                    if state_str == "stopped" {
                        stopped_cids.insert(cid.to_string());
                        // Update this specific service to stopped in DB
                        let _ = sqlx::query("UPDATE project_services SET status = 'stopped' WHERE project_id = ? AND container_id = ?")
                            .bind(&project_id)
                            .bind(cid)
                            .execute(&state.db)
                            .await;
                        caddy_dirty = true;
                        continue;
                    }

                    let cpu = item["cpu_percent"].as_f64().unwrap_or(0.0);
                    let mem_usage = item["memory_usage"].as_u64().unwrap_or(0);
                    let mem_limit = item["memory_limit"].as_u64().unwrap_or(0);
                    let cpu_limit = item["cpu_limit"].as_f64();

                    per_container.insert(cid.to_string(), (cpu, mem_usage, mem_limit, disk_gb, cpu_limit));

                    let entry = project_stats.entry(project_id.clone()).or_insert((0.0, 0, 0, 0.0));
                    entry.0 += cpu;
                    entry.1 += mem_usage;
                    entry.2 += mem_limit;
                    entry.3 += disk_gb;
                }
            }
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: failed to parse response");
            }
        }

        // Build results for each project on this node
        for (project_id, _) in projects_containers {
            let services_raw = services_map.get(project_id).cloned().unwrap_or_default();
            if project_stats.contains_key(project_id) {
                let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
                results.push(make_stats_response(
                    project_id.clone(),
                    "running".to_string(),
                    services,
                ));
                // Mark degraded if some services stopped
                if !stopped_cids.is_empty() {
                    let now = chrono::Utc::now().timestamp();
                    let _ = sqlx::query("UPDATE projects SET status = 'degraded', updated_at = ? WHERE id = ? AND status != 'degraded'")
                        .bind(now)
                        .bind(project_id)
                        .execute(&state.db)
                        .await;
                    if !caddy_dirty { caddy_dirty = true; }
                }
            } else {
                // All containers were stopped — mark project as stopped
                let now = chrono::Utc::now().timestamp();
                let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(project_id)
                    .execute(&state.db)
                    .await;
                let services: Vec<ServiceInfo> = services_raw.into_iter().map(|(mut svc, cid)| {
                    if let Some(container_id) = cid {
                        if let Some(bytes) = state.disk_cache.get(&container_id) {
                            svc.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                        }
                    }
                    svc
                }).collect();
                results.push(make_stats_response(
                    project_id.clone(),
                    "stopped".to_string(),
                    services,
                ));
            }
        }
    }

    if caddy_dirty {
        sync_caddy(&state).await;
    }

    Ok(Json(BatchStatsResponse { stats: results }))
}

/// GET /projects/:id/stats
pub async fn project_stats(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<StatsResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    // Load services for this project
    let services_raw = batch_load_services(&state.db, &[project_id.clone()])
        .await
        .remove(&project_id)
        .unwrap_or_default();

    if project.status != "running" {
        return Ok(Json(make_stats_response(
            project_id,
            project.status,
            services_raw.into_iter().map(|(s, _)| s).collect(),
        )));
    }

    // Get all running container IDs for this project (multi-service aware)
    let (container_ids, _) = project_container_ids(&state.db, &project).await;
    if container_ids.is_empty() {
        return Ok(Json(make_stats_response(
            project_id,
            project.status,
            services_raw.into_iter().map(|(s, _)| s).collect(),
        )));
    }

    // Per-service stats breakdown
    let mut any_running = false;
    let mut per_container: std::collections::HashMap<String, LiveStats> = std::collections::HashMap::new();
    let mut stopped_cids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for cid in &container_ids {
        let actually_running = state.docker.is_container_running(cid).await.unwrap_or(false);
        if !actually_running {
            stopped_cids.insert(cid.clone());
            // Update this specific service to stopped in DB
            let _ = sqlx::query("UPDATE project_services SET status = 'stopped' WHERE project_id = ? AND container_id = ?")
                .bind(&project_id)
                .bind(cid)
                .execute(&state.db)
                .await;
            continue;
        }
        any_running = true;

        let stats_fut = state.docker.container_stats(cid);
        let disk_fut = state.docker.disk_usage(cid);
        let (stats_res, disk_res) = tokio::join!(stats_fut, disk_fut);

        let (cpu, mem_usage, mem_limit) = match stats_res {
            Ok(s) => (s.cpu_percent, s.memory_usage, s.memory_limit),
            Err(_) => (0.0, 0, 0),
        };

        let (disk, cpu_limit) = match disk_res {
            Ok(d) => (d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0), d.cpu_limit),
            Err(_) => (0.0, None),
        };

        per_container.insert(cid.clone(), (cpu, mem_usage, mem_limit, disk, cpu_limit));
    }

    if !any_running {
        let now = chrono::Utc::now().timestamp();
        let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await;
        sync_caddy(&state).await;

        return Ok(Json(make_stats_response(
            project_id,
            "stopped".to_string(),
            services_raw.into_iter().map(|(s, _)| s).collect(),
        )));
    }

    let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
    Ok(Json(make_stats_response(
        project_id,
        project.status,
        services,
    )))
}

/// GET /projects/:id/disk-usage
pub async fn project_disk_usage(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<DiskUsageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status != "running" {
        return Ok(Json(DiskUsageResponse {
            project_id,
            size_gb: 0.0,
        }));
    }

    let container_id = project
        .container_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?;

    let is_remote = project
        .node_id
        .as_deref()
        .map(|n| n != "local")
        .unwrap_or(false);

    let usage = if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let resp = client
            .get(&format!("{}/containers/{}/disk-usage", base_url, container_id))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent disk-usage failed: {body}")));
        }

        resp.json().await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse response: {e}")))?
    } else {
        state.docker.disk_usage(container_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("disk-usage error: {e}")))?
    };

    let size_gb = usage.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(Json(DiskUsageResponse { project_id, size_gb }))
}

/// GET /projects/:id/logs?tail=100&service=frontend
/// For multi-service projects, `service` selects a specific service's logs.
/// Defaults to the public service if not specified.
pub async fn project_logs(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    let tail = query.tail.unwrap_or(100);

    // Resolve the container_id to tail logs from
    let (container_id, service_name) = if project.service_count.unwrap_or(1) > 1 {
        // Multi-service: look up specific service or fall back to public service
        if let Some(ref svc) = query.service {
            let row: Option<(Option<String>,)> = sqlx::query_as(
                "SELECT container_id FROM project_services WHERE project_id = ? AND service_name = ?"
            )
            .bind(&project_id)
            .bind(svc)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            match row.and_then(|(cid,)| cid) {
                Some(cid) => (cid, Some(svc.clone())),
                None => return Err((StatusCode::NOT_FOUND, format!("service '{}' not found", svc))),
            }
        } else {
            // Default to public service
            let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT container_id, service_name FROM project_services WHERE project_id = ? AND is_public = 1"
            )
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            match row.and_then(|(cid, name)| cid.map(|c| (c, name))) {
                Some((cid, name)) => (cid, name),
                None => {
                    // No public service — try first service
                    let row2: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                        "SELECT container_id, service_name FROM project_services WHERE project_id = ? AND container_id IS NOT NULL LIMIT 1"
                    )
                    .bind(&project_id)
                    .fetch_optional(&state.db)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
                    match row2.and_then(|(cid, name)| cid.map(|c| (c, name))) {
                        Some((cid, name)) => (cid, name),
                        None => return Err((StatusCode::BAD_REQUEST, "no running service containers".to_string())),
                    }
                }
            }
        }
    } else {
        // Single-service
        let cid = project.container_id.as_deref()
            .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?
            .to_string();
        (cid, None)
    };

    let is_remote = project
        .node_id
        .as_deref()
        .map(|n| n != "local")
        .unwrap_or(false);

    let lines = if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        let resp = client
            .get(&format!("{}/containers/{}/logs?tail={}", base_url, container_id, tail))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent logs failed: {body}")));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to read log body: {e}")))?;

        body.lines().map(|l| l.to_string()).collect()
    } else {
        state
            .docker
            .container_logs(&container_id, tail)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("logs error: {e}")))?
    };

    Ok(Json(LogsResponse {
        project_id,
        service_name,
        lines,
    }))
}
