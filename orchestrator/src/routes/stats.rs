use axum::{extract::Path, extract::Query, extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::nodes;
use crate::AppState;
use super::manage::{agent_base_url, get_node_from_db, sync_caddy};

#[derive(Serialize)]
pub struct StatsResponse {
    pub project_id: String,
    pub status: String,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub disk_gb: f64,
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
}

#[derive(Serialize)]
pub struct LogsResponse {
    pub project_id: String,
    pub lines: Vec<String>,
}

/// GET /projects/stats — returns stats + disk for all running projects in one call
pub async fn all_project_stats(
    State(state): State<AppState>,
) -> Result<Json<BatchStatsResponse>, (StatusCode, String)> {
    let projects = sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    let mut results: Vec<StatsResponse> = Vec::with_capacity(projects.len());
    let mut caddy_dirty = false;

    let mut local_containers: Vec<(String, String)> = Vec::new();
    let mut remote_by_node: std::collections::HashMap<String, Vec<(String, String)>> = std::collections::HashMap::new();

    for project in &projects {
        if project.status != "running" {
            let container_id = match project.container_id.as_deref() {
                Some(id) => id.to_string(),
                None => {
                    results.push(StatsResponse {
                        project_id: project.id.clone(),
                        status: project.status.clone(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: 0.0,
                    });
                    continue;
                }
            };

            // Use cached disk if available
            if let Some(bytes) = state.disk_cache.get(&project.id) {
                results.push(StatsResponse {
                    project_id: project.id.clone(),
                    status: project.status.clone(),
                    cpu_percent: 0.0,
                    memory_usage: 0,
                    memory_limit: 0,
                    disk_gb: *bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                });
            } else {
                // Cache miss — include in agent batch to fetch disk
                let node_id = project.node_id.as_deref().unwrap_or("local");
                if node_id == "local" {
                    local_containers.push((project.id.clone(), container_id));
                } else {
                    remote_by_node
                        .entry(node_id.to_string())
                        .or_default()
                        .push((project.id.clone(), container_id));
                }
            }
            continue;
        }

        let container_id = match project.container_id.as_deref() {
            Some(id) => id.to_string(),
            None => {
                results.push(StatsResponse {
                    project_id: project.id.clone(),
                    status: project.status.clone(),
                    cpu_percent: 0.0,
                    memory_usage: 0,
                    memory_limit: 0,
                    disk_gb: 0.0,
                });
                continue;
            }
        };

        let node_id = project.node_id.as_deref().unwrap_or("local");
        if node_id == "local" {
            local_containers.push((project.id.clone(), container_id));
        } else {
            remote_by_node
                .entry(node_id.to_string())
                .or_default()
                .push((project.id.clone(), container_id));
        }
    }

    // Fetch local stats (running containers)
    for (project_id, container_id) in &local_containers {
        let actually_running = state.docker.is_container_running(container_id).await.unwrap_or(false);
        if !actually_running {
            let now = chrono::Utc::now().timestamp();
            // Container still exists on disk — query and cache before marking stopped
            if let Ok(d) = state.docker.disk_usage(container_id).await {
                state.disk_cache.insert(project_id.clone(), d.size_root_fs as i64);
            }
            let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(project_id)
                .execute(&state.db)
                .await;
            caddy_dirty = true;

            let disk_gb = state.disk_cache.get(project_id)
                .map(|bytes| *bytes as f64 / (1024.0 * 1024.0 * 1024.0))
                .unwrap_or(0.0);
            results.push(StatsResponse {
                project_id: project_id.clone(),
                status: "stopped".to_string(),
                cpu_percent: 0.0,
                memory_usage: 0,
                memory_limit: 0,
                disk_gb,
            });
            continue;
        }

        let stats_fut = state.docker.container_stats(container_id);
        let disk_fut = state.docker.disk_usage(container_id);
        let (stats_res, disk_res) = tokio::join!(stats_fut, disk_fut);

        let stats = match stats_res {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(project = %project_id, error = %e, "batch stats: failed to fetch local stats");
                results.push(StatsResponse {
                    project_id: project_id.clone(),
                    status: "running".to_string(),
                    cpu_percent: 0.0,
                    memory_usage: 0,
                    memory_limit: 0,
                    disk_gb: 0.0,
                });
                continue;
            }
        };

        let disk_gb = match disk_res {
            Ok(d) => {
                state.disk_cache.insert(project_id.clone(), d.size_root_fs as i64);
                d.size_root_fs as f64 / (1024.0 * 1024.0 * 1024.0)
            }
            Err(_) => 0.0,
        };

        results.push(StatsResponse {
            project_id: project_id.clone(),
            status: "running".to_string(),
            cpu_percent: stats.cpu_percent,
            memory_usage: stats.memory_usage,
            memory_limit: stats.memory_limit,
            disk_gb,
        });
    }

    // Fetch remote stats — one POST per node with all container IDs
    for (node_id, containers) in &remote_by_node {
        let container_ids: Vec<String> = containers.iter().map(|(_, cid)| cid.clone()).collect();

        let client = match nodes::client::get_node_client(&state.node_clients, node_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: node client unavailable");
                for (project_id, _) in containers {
                    results.push(StatsResponse {
                        project_id: project_id.clone(),
                        status: "running".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: 0.0,
                    });
                }
                continue;
            }
        };

        let node = match get_node_from_db(&state.db, node_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = ?e, "batch stats: node not found");
                for (project_id, _) in containers {
                    results.push(StatsResponse {
                        project_id: project_id.clone(),
                        status: "running".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: 0.0,
                    });
                }
                continue;
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        let resp = match client
            .post(format!("{}/containers/stats", base_url))
            .json(&json!({ "container_ids": container_ids }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: agent unreachable");
                for (project_id, _) in containers {
                    results.push(StatsResponse {
                        project_id: project_id.clone(),
                        status: "running".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: 0.0,
                    });
                }
                continue;
            }
        };

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(node_id = %node_id, body = %body, "batch stats: agent returned error");
            for (project_id, _) in containers {
                results.push(StatsResponse {
                    project_id: project_id.clone(),
                    status: "running".to_string(),
                    cpu_percent: 0.0,
                    memory_usage: 0,
                    memory_limit: 0,
                    disk_gb: 0.0,
                });
            }
            continue;
        }

        let container_id_to_project: std::collections::HashMap<String, String> =
            containers.iter().map(|(pid, cid)| (cid.clone(), pid.clone())).collect();

        match resp.json::<Vec<serde_json::Value>>().await {
            Ok(items) => {
                for item in &items {
                    let cid = item["container_id"].as_str().unwrap_or("");
                    let project_id = container_id_to_project.get(cid)
                        .cloned()
                        .unwrap_or_default();
                    let state_str = item["state"].as_str().unwrap_or("running");
                    let disk_gb = item["disk_gb"].as_f64().unwrap_or(0.0);

                    // Cache disk bytes from agent response
                    if disk_gb > 0.0 {
                        state.disk_cache.insert(project_id.clone(), (disk_gb * 1024.0 * 1024.0 * 1024.0) as i64);
                    }

                    if state_str == "stopped" {
                        let now = chrono::Utc::now().timestamp();
                        let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                            .bind(now)
                            .bind(&project_id)
                            .execute(&state.db)
                            .await;
                        caddy_dirty = true;

                        results.push(StatsResponse {
                            project_id,
                            status: "stopped".to_string(),
                            cpu_percent: 0.0,
                            memory_usage: 0,
                            memory_limit: 0,
                            disk_gb,
                        });
                    } else {
                        results.push(StatsResponse {
                            project_id,
                            status: "running".to_string(),
                            cpu_percent: item["cpu_percent"].as_f64().unwrap_or(0.0),
                            memory_usage: item["memory_usage"].as_u64().unwrap_or(0),
                            memory_limit: item["memory_limit"].as_u64().unwrap_or(0),
                            disk_gb,
                        });
                    }
                }
            }
            Err(e) => {
                tracing::warn!(node_id = %node_id, error = %e, "batch stats: failed to parse response");
                for (project_id, _) in containers {
                    results.push(StatsResponse {
                        project_id: project_id.clone(),
                        status: "running".to_string(),
                        cpu_percent: 0.0,
                        memory_usage: 0,
                        memory_limit: 0,
                        disk_gb: 0.0,
                    });
                }
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

    if project.status != "running" {
        return Ok(Json(StatsResponse {
            project_id,
            status: project.status,
            cpu_percent: 0.0,
            memory_usage: 0,
            memory_limit: 0,
            disk_gb: 0.0,
        }));
    }

    let container_id = project
        .container_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?;

    // Check if the container is actually running — detect external stops
    let actually_running = state
        .docker
        .is_container_running(container_id)
        .await
        .unwrap_or(false);

    if !actually_running {
        let now = chrono::Utc::now().timestamp();
        let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&project_id)
            .execute(&state.db)
            .await;

        sync_caddy(&state).await;

        tracing::info!(project = %project_id, "stats: detected externally stopped container");

        return Ok(Json(StatsResponse {
            project_id,
            status: "stopped".to_string(),
            cpu_percent: 0.0,
            memory_usage: 0,
            memory_limit: 0,
            disk_gb: 0.0,
        }));
    }

    let stats = state
        .docker
        .container_stats(container_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stats error: {e}")))?;

    Ok(Json(StatsResponse {
        project_id,
        status: project.status,
        cpu_percent: stats.cpu_percent,
        memory_usage: stats.memory_usage,
        memory_limit: stats.memory_limit,
        disk_gb: 0.0,
    }))
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

/// GET /projects/:id/logs?tail=100
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

    let container_id = project
        .container_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?
        .to_string();

    let tail = query.tail.unwrap_or(100);

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
        lines,
    }))
}
