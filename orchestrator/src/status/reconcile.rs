use sqlx::SqlitePool;
use tracing::debug;

use litebin_common::docker::DockerManager;
use litebin_common::types::ProjectStatus;

use super::derive::{derive_and_set_project_status, refresh_oneshot_flags};
use super::transition::sync_single_service_row;

// ---------------------------------------------------------------------------
// sync_from_docker() — reconciliation with actual Docker state
// ---------------------------------------------------------------------------

/// Result of a sync pass for one project.
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub old_status: ProjectStatus,
    pub new_status: ProjectStatus,
    pub caddy_dirty: bool,
}

/// Sync a single local project's status from actual Docker container state.
///
/// Checks each service's container via `is_container_running()`, updates
/// `project_services.status` to match Docker, then derives `projects.status`.
///
/// Skips transient/setup states (pending, unconfigured, deploying, stopping, error).
pub async fn sync_project_from_docker(db: &SqlitePool, docker: &DockerManager, project_id: &str) -> SyncResult {
    // Load current project status
    let current_status: ProjectStatus = sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .unwrap_or(ProjectStatus::Error);

    // Skip transient states — these are managed by their owning code paths
    if current_status.is_transient() {
        return SyncResult { old_status: current_status.clone(), new_status: current_status, caddy_dirty: false };
    }

    // Load services
    let services: Vec<(String, Option<String>, bool)> = match sqlx::query_as(
        "SELECT service_name, container_id, is_oneshot FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "status: failed to fetch services for sync, skipping");
            return SyncResult { old_status: current_status.clone(), new_status: current_status, caddy_dirty: false };
        }
    };

    if services.is_empty() {
        return SyncResult { old_status: current_status.clone(), new_status: current_status, caddy_dirty: false };
    }

    refresh_oneshot_flags(db, project_id).await;

    // Re-load after oneshot refresh
    let services: Vec<(String, Option<String>, bool)> = match sqlx::query_as(
        "SELECT service_name, container_id, is_oneshot FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "status: failed to re-fetch services for sync");
            return SyncResult { old_status: current_status.clone(), new_status: current_status, caddy_dirty: false };
        }
    };

    // Check each service's container against Docker
    let mut running_count = 0i32;
    for (service_name, container_id, is_oneshot) in &services {
        let actually_running = match container_id {
            Some(cid) if !cid.is_empty() => docker.is_container_running(cid).await.unwrap_or(false),
            _ => false,
        };

        let new_svc_status = if actually_running {
            ProjectStatus::Running
        } else if *is_oneshot {
            let exit_ok = match container_id {
                Some(cid) if !cid.is_empty() => {
                    matches!(docker.container_exit_code(cid).await.ok().flatten(), Some(0))
                }
                _ => false,
            };
            if exit_ok { ProjectStatus::Completed } else { ProjectStatus::Stopped }
        } else {
            ProjectStatus::Stopped
        };

        if actually_running {
            running_count += 1;
        }

        // Fix stale service status
        if let Err(e) = sqlx::query(
            "UPDATE project_services SET status = ? WHERE project_id = ? AND service_name = ? AND status != ?",
        )
        .bind(&new_svc_status)
        .bind(project_id)
        .bind(service_name)
        .bind(&new_svc_status)
        .execute(db)
        .await
        {
            tracing::warn!(project_id = %project_id, service = %service_name, error = %e, "status: failed to fix stale service status");
        }
    }

    // Fallback for single-service projects: the waker may have created a new container
    // and updated projects.container_id but not project_services.container_id.
    // If all services appear stopped but projects.container_id is running, fix the row.
    if running_count == 0 && services.len() == 1 {
        let projects_cid: Option<String> = sqlx::query_scalar("SELECT container_id FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_one(db)
            .await
            .ok()
            .flatten();

        if let Some(ref cid) = projects_cid
            && !cid.is_empty()
            && docker.is_container_running(cid).await.unwrap_or(false)
        {
            let port: Option<i64> = sqlx::query_scalar("SELECT mapped_port FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_one(db)
                .await
                .unwrap_or(None);

            sync_single_service_row(db, project_id, cid, port.unwrap_or(0)).await;
        }
    }

    // Derive project status from aggregated service states
    let new_status = derive_and_set_project_status(db, project_id).await;
    let caddy_dirty = new_status != current_status;

    SyncResult { old_status: current_status, new_status, caddy_dirty }
}

/// Batch-sync all **local** projects from Docker state.
/// Skips transient states, remote-node projects, and projects with no container_ids.
pub async fn sync_all_local_from_docker(db: &SqlitePool, docker: &DockerManager) -> Vec<SyncResult> {
    let project_ids: Vec<String> = match sqlx::query_scalar(
        "SELECT DISTINCT ps.project_id
         FROM project_services ps
         JOIN projects p ON p.id = ps.project_id
         WHERE ps.container_id IS NOT NULL AND ps.container_id != ''
           AND (p.node_id IS NULL OR p.node_id = 'local')",
    )
    .fetch_all(db)
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "status: failed to fetch project IDs for sync");
            Vec::new()
        }
    };

    let mut changed = Vec::new();
    for pid in &project_ids {
        let result = sync_project_from_docker(db, docker, pid).await;
        if result.caddy_dirty {
            debug!(
                project = %pid,
                old = %result.old_status,
                new = %result.new_status,
                "sync: corrected project status"
            );
            changed.push(result);
        }
    }

    changed
}

/// Update a project's status from agent-reported container states.
/// Used by the stats endpoint for remote projects.
///
/// Takes parsed container states from the agent's `/containers/stats` response.
/// Skips transient states.
pub async fn update_status_from_container_states(
    db: &SqlitePool,
    project_id: &str,
    container_states: &[(String, bool)], // (container_id, is_running)
) -> SyncResult {
    let current_status: ProjectStatus = sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .unwrap_or(ProjectStatus::Error);

    if current_status.is_transient() {
        return SyncResult { old_status: current_status.clone(), new_status: current_status, caddy_dirty: false };
    }

    for (container_id, is_running) in container_states {
        let oneshot: bool =
            sqlx::query_scalar("SELECT is_oneshot FROM project_services WHERE project_id = ? AND container_id = ?")
                .bind(project_id)
                .bind(container_id)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .unwrap_or(false);

        let new_svc_status = if *is_running {
            ProjectStatus::Running
        } else if oneshot {
            ProjectStatus::Completed
        } else {
            ProjectStatus::Stopped
        };

        if let Err(e) = sqlx::query(
            "UPDATE project_services SET status = ? WHERE project_id = ? AND container_id = ? AND status != ?",
        )
        .bind(&new_svc_status)
        .bind(project_id)
        .bind(container_id)
        .bind(&new_svc_status)
        .execute(db)
        .await
        {
            tracing::warn!(project_id = %project_id, container_id = %container_id, error = %e, "status: failed to update service status from container state");
        }
    }

    // Derive project status from aggregated service states
    let new_status = derive_and_set_project_status(db, project_id).await;
    let caddy_dirty = new_status != current_status;

    SyncResult { old_status: current_status, new_status, caddy_dirty }
}
