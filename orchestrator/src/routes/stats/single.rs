use axum::{Json, extract::Path, extract::State, http::StatusCode};

use super::super::manage::sync_caddy;
use super::helpers::{
    batch_load_services, enrich_services, load_project_container_ids, make_stats_response, project_container_ids,
};
use super::types::{DiskUsageResponse, LiveStats, StatsResponse};
use crate::AppState;
use crate::status;
use litebin_common::types::ProjectStatus;

#[utoipa::path(
    get,
    path = "/projects/{id}/stats",
    description = "Per-service CPU, memory, and disk stats for one project (syncs status from Docker first).",
    params(
        ("id" = String, Path, description = "Project ID"),
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
    let node_id = project.node_id.clone().unwrap_or_else(|| "local".to_string());
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
    let mut services_raw =
        batch_load_services(&state.db, std::slice::from_ref(&project_id)).await.remove(&project_id).unwrap_or_default();
    super::helpers::apply_data_usage(
        &state,
        &project_id,
        if node_id == "local" { None } else { Some(&node_id) },
        &mut services_raw,
    )
    .await;

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

    let services = enrich_services(&services_raw, &per_container, &stopped_cids);
    Ok(Json(make_stats_response(project_id, project.status, project.last_active_at, services)))
}

#[utoipa::path(
    get,
    path = "/projects/{id}/disk-usage",
    description = "Total disk usage (GB) across a running project's containers.",
    params(
        ("id" = String, Path, description = "Project ID"),
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

    let node = project.node_id.as_deref().unwrap_or("local");

    let volumes = super::helpers::scoped_volume_list(&state.db, &project_id, Some("total")).await;
    let mut total: i64 = 0;
    for (scoped, _) in volumes {
        if litebin_common::types::classify_volume(&scoped) != litebin_common::types::VolumeKind::AbsoluteBindMount
            && let Some(bytes) = super::helpers::volume_bytes(&state, node, &scoped).await
        {
            total += bytes;
        }
    }
    for cid in load_project_container_ids(&state.db, &project).await.unwrap_or_default() {
        if let Some((_, root_fs)) = super::helpers::container_disk(&state, node, &cid).await {
            total += root_fs;
        }
    }

    Ok(Json(DiskUsageResponse { project_id, size_gb: total as f64 / (1024.0 * 1024.0 * 1024.0) }))
}
