use axum::{Json, extract::Path, extract::State, http::StatusCode};

use super::super::manage::{agent_base_url, get_node_from_db, sync_caddy};
use super::helpers::{
    aggregate_root_fs_bytes, batch_load_services, enrich_services, load_project_container_ids, make_stats_response,
    project_container_ids,
};
use super::types::{DiskUsageResponse, LiveStats, StatsResponse};
use crate::AppState;
use crate::nodes;
use crate::status;
use litebin_common::types::{DeployType, ProjectStatus};

/// GET /projects/:id/stats
#[utoipa::path(
    get,
    path = "/projects/{project_id}/stats",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = StatsResponse),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn project_stats(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<StatsResponse>, (StatusCode, String)> {
    let mut project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    // Sync status from Docker for local projects
    let node_id = project.node_id.as_deref().unwrap_or("local");
    if node_id == "local" {
        let sync_result = status::sync_project_from_docker(&state.db, &state.docker, &project_id).await;
        if sync_result.caddy_dirty {
            sync_caddy(&state).await;
        }
        // Re-read project to get updated status
        project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    }

    // Load services for this project (after sync so services are fresh)
    let services_raw =
        batch_load_services(&state.db, std::slice::from_ref(&project_id)).await.remove(&project_id).unwrap_or_default();

    if project.status != ProjectStatus::Running {
        return Ok(Json(make_stats_response(
            project_id,
            project.status,
            project.last_active_at,
            services_raw.into_iter().map(|(s, _)| s).collect(),
        )));
    }

    // Get all running container IDs for this project (multi-service aware)
    let (container_ids, _) = project_container_ids(&state.db, &project).await;
    if container_ids.is_empty() {
        return Ok(Json(make_stats_response(
            project_id,
            project.status,
            project.last_active_at,
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
        return Ok(Json(make_stats_response(
            project_id,
            ProjectStatus::Stopped,
            project.last_active_at,
            services_raw.into_iter().map(|(s, _)| s).collect(),
        )));
    }

    let services = enrich_services(&services_raw, &per_container, &stopped_cids, &state.disk_cache);
    Ok(Json(make_stats_response(project_id, project.status, project.last_active_at, services)))
}

/// GET /projects/:id/disk-usage
#[utoipa::path(
    get,
    path = "/projects/{project_id}/disk-usage",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = DiskUsageResponse),
        (status = 404, description = "Project not found"),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn project_disk_usage(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<DiskUsageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.status != ProjectStatus::Running {
        return Ok(Json(DiskUsageResponse { project_id, size_gb: 0.0 }));
    }

    let container_ids = load_project_container_ids(&state.db, &project)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
    if container_ids.is_empty() {
        let message = if project.deploy_type == Some(DeployType::Compose) {
            "no active service container ids"
        } else {
            "no container id"
        };
        return Err((StatusCode::BAD_REQUEST, message.to_string()));
    }

    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    let mut root_fs_sizes = Vec::with_capacity(container_ids.len());
    if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let base_url = agent_base_url(&state.config, &node);

        for container_id in &container_ids {
            let resp = client
                .get(format!("{}/containers/{}/disk-usage", base_url, container_id))
                .send()
                .await
                .map_err(|e| {
                    (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable for container '{container_id}': {e}"))
                })?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("agent disk-usage failed for container '{container_id}': {body}"),
                ));
            }

            let usage: litebin_common::docker::DiskUsage = resp.json().await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to parse disk-usage response for container '{container_id}': {e}"),
                )
            })?;
            root_fs_sizes.push(usage.size_root_fs);
        }
    } else {
        for container_id in &container_ids {
            let usage = state.docker.disk_usage(container_id).await.map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("disk-usage error for container '{container_id}': {e}"))
            })?;
            root_fs_sizes.push(usage.size_root_fs);
        }
    }

    let total_bytes =
        aggregate_root_fs_bytes(root_fs_sizes).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let size_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    Ok(Json(DiskUsageResponse { project_id, size_gb }))
}
