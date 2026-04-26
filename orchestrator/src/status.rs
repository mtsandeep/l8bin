use sqlx::SqlitePool;
use tracing::{debug, warn};

use litebin_common::docker::DockerManager;

/// Transient states that should NOT be overridden by sync_from_docker.
/// These are intentional states managed by their owning code paths.
const TRANSIENT_STATUSES: &[&str] = &["deploying", "stopping", "error", "unconfigured"];

// ---------------------------------------------------------------------------
// transition() — intentional state changes
// ---------------------------------------------------------------------------

/// Optional fields to update alongside status on the `projects` row.
#[derive(Debug, Clone, Default)]
pub struct ProjectUpdateFields {
    pub container_id: Option<Option<String>>,
    pub mapped_port: Option<Option<i64>>,
    pub node_id: Option<String>,
    pub last_active_at: Option<i64>,
}

/// Transition a project's status atomically across both tables.
///
/// This is the SOLE entry point for intentional status changes.
/// It runs inside a SQLite transaction so both tables are always consistent.
///
/// # Consistency Rules
/// - `Stopped` / `Stopping` / `Error` / `Deploying` → cascade to ALL services
/// - `Running` → cascade to ALL services (or filtered set if `service_filter` provided)
/// - `Degraded` → update `projects` only (degraded is derived from per-service states)
pub async fn transition(
    db: &SqlitePool,
    project_id: &str,
    new_status: &str,
    extra: &ProjectUpdateFields,
    service_filter: Option<&[String]>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut tx = db.begin().await?;

    // 1. Update projects table using QueryBuilder for dynamic fields
    let mut qb = sqlx::QueryBuilder::new("UPDATE projects SET ");
    qb.push("status = ").push_bind(new_status);
    qb.push(", updated_at = ").push_bind(now);

    if let Some(ref cid) = extra.container_id {
        qb.push(", container_id = ");
        if let Some(id) = cid {
            qb.push_bind(id.as_str());
        } else {
            qb.push("NULL");
        }
    }
    if let Some(ref port) = extra.mapped_port {
        qb.push(", mapped_port = ");
        if let Some(p) = port {
            qb.push_bind(*p);
        } else {
            qb.push("NULL");
        }
    }
    if let Some(ref nid) = extra.node_id {
        qb.push(", node_id = ").push_bind(nid.as_str());
    }
    if let Some(laa) = extra.last_active_at {
        qb.push(", last_active_at = ").push_bind(laa);
    }
    qb.push(" WHERE id = ").push_bind(project_id);

    qb.build().execute(&mut *tx).await?;

    // 2. Update project_services table
    match new_status {
        s if s == "deploying" || s == "running" => {
            if let Some(services) = service_filter {
                for svc_name in services {
                    sqlx::query(
                        "UPDATE project_services SET status = ? WHERE project_id = ? AND service_name = ?",
                    )
                    .bind(new_status)
                    .bind(project_id)
                    .bind(svc_name)
                    .execute(&mut *tx)
                    .await?;
                }
            } else {
                sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ?")
                    .bind(new_status)
                    .bind(project_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        s if s == "stopped" || s == "stopping" || s == "error" => {
            // Always cascade to ALL services for terminal/error states
            sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ?")
                .bind(new_status)
                .bind(project_id)
                .execute(&mut *tx)
                .await?;
        }
        "degraded" => {
            // Do NOT touch services — degraded is derived from individual service states
        }
        _ => {
            warn!(status = new_status, "transition: unknown status, skipping service cascade");
        }
    }

    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-service helpers
// ---------------------------------------------------------------------------

/// Set a specific service to running with container info.
/// Used by deploy success, start, and recreate paths.
pub async fn set_service_running(
    db: &SqlitePool,
    project_id: &str,
    service_name: &str,
    container_id: &str,
    mapped_port: Option<i64>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE project_services SET status = 'running', container_id = ?, mapped_port = ? WHERE project_id = ? AND service_name = ?",
    )
    .bind(container_id)
    .bind(mapped_port)
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark a service as stopped, clearing its mapped_port.
/// Keeps container_id so the disk cache (keyed by container_id) remains valid.
/// Used by rollback, recreate cleanup, and partial redeploy.
pub async fn set_service_stopped(
    db: &SqlitePool,
    project_id: &str,
    service_name: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE project_services SET status = 'stopped', mapped_port = NULL WHERE project_id = ? AND service_name = ?",
    )
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Update the single-service "web" row in project_services to match projects table.
/// Single-service projects have a "web" row that must track container_id/mapped_port.
/// Called by waker and handlers after creating or starting a single-service container.
pub async fn sync_single_service_row(
    db: &SqlitePool,
    project_id: &str,
    container_id: &str,
    mapped_port: i64,
) {
    let _ = sqlx::query(
        "UPDATE project_services SET status = 'running', container_id = ?, mapped_port = ? WHERE project_id = ? AND service_name = 'web'",
    )
    .bind(container_id)
    .bind(mapped_port)
    .bind(project_id)
    .execute(db)
    .await;
}

/// Derive project status from aggregated service states and update projects table.
/// Returns the derived status string.
///
/// - All services running → "running"
/// - Some services running → "degraded"
/// - No services running → "stopped"
pub async fn derive_and_set_project_status(db: &SqlitePool, project_id: &str) -> String {
    let statuses: Vec<(String,)> = sqlx::query_as(
        "SELECT status FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if statuses.is_empty() {
        return "stopped".to_string();
    }

    let total = statuses.len();
    let running = statuses.iter().filter(|(s,)| s == "running").count();
    let new_status = if running == total {
        "running"
    } else if running > 0 {
        "degraded"
    } else {
        "stopped"
    };

    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
        .bind(new_status)
        .bind(now)
        .bind(project_id)
        .execute(db)
        .await;

    new_status.to_string()
}

// ---------------------------------------------------------------------------
// sync_from_docker() — reconciliation with actual Docker state
// ---------------------------------------------------------------------------

/// Result of a sync pass for one project.
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub old_status: String,
    pub new_status: String,
    pub caddy_dirty: bool,
}

/// Sync a single local project's status from actual Docker container state.
///
/// Checks each service's container via `is_container_running()`, updates
/// `project_services.status` to match Docker, then derives `projects.status`.
///
/// Skips transient states (deploying, stopping, error, unconfigured).
pub async fn sync_project_from_docker(
    db: &SqlitePool,
    docker: &DockerManager,
    project_id: &str,
) -> SyncResult {
    // Load current project status
    let current_status: String = sqlx::query_scalar(
        "SELECT status FROM projects WHERE id = ?",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or_else(|_| "error".into());

    // Skip transient states — these are managed by their owning code paths
    if TRANSIENT_STATUSES.contains(&current_status.as_str()) {
        return SyncResult {
            old_status: current_status.clone(),
            new_status: current_status,
            caddy_dirty: false,
        };
    }

    // Load services
    let services: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT service_name, container_id FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if services.is_empty() {
        return SyncResult {
            old_status: current_status.clone(),
            new_status: current_status,
            caddy_dirty: false,
        };
    }

    // Check each service's container against Docker
    let mut running_count = 0i32;
    for (service_name, container_id) in &services {
        let actually_running = match container_id {
            Some(cid) if !cid.is_empty() => docker.is_container_running(cid).await.unwrap_or(false),
            _ => false,
        };
        let new_svc_status = if actually_running { "running" } else { "stopped" };

        if actually_running {
            running_count += 1;
        }

        // Fix stale service status
        let _ = sqlx::query(
            "UPDATE project_services SET status = ? WHERE project_id = ? AND service_name = ? AND status != ?",
        )
        .bind(new_svc_status)
        .bind(project_id)
        .bind(service_name)
        .bind(new_svc_status)
        .execute(db)
        .await;
    }

    // Fallback for single-service projects: the waker may have created a new container
    // and updated projects.container_id but not project_services.container_id.
    // If all services appear stopped but projects.container_id is running, fix the row.
    if running_count == 0 && services.len() == 1 {
        let projects_cid: Option<String> = sqlx::query_scalar(
            "SELECT container_id FROM projects WHERE id = ?",
        )
        .bind(project_id)
        .fetch_one(db)
        .await
        .ok()
        .flatten();

        if let Some(ref cid) = projects_cid {
            if !cid.is_empty() && docker.is_container_running(cid).await.unwrap_or(false) {
                let port: Option<i64> = sqlx::query_scalar(
                    "SELECT mapped_port FROM projects WHERE id = ?",
                )
                .bind(project_id)
                .fetch_one(db)
                .await
                .unwrap_or(None);

                sync_single_service_row(db, project_id, cid, port.unwrap_or(0)).await;
            }
        }
    }

    // Derive project status from aggregated service states
    let new_status = derive_and_set_project_status(db, project_id).await;
    let caddy_dirty = new_status != current_status;

    SyncResult {
        old_status: current_status,
        new_status,
        caddy_dirty,
    }
}

/// Batch-sync all local projects from Docker state.
/// Skips transient states and projects with no container_ids.
pub async fn sync_all_local_from_docker(
    db: &SqlitePool,
    docker: &DockerManager,
) -> Vec<SyncResult> {
    let project_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT project_id FROM project_services WHERE container_id IS NOT NULL AND container_id != ''",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

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
    let current_status: String = sqlx::query_scalar(
        "SELECT status FROM projects WHERE id = ?",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or_else(|_| "error".into());

    if TRANSIENT_STATUSES.contains(&current_status.as_str()) {
        return SyncResult {
            old_status: current_status.clone(),
            new_status: current_status,
            caddy_dirty: false,
        };
    }

    for (container_id, is_running) in container_states {
        let new_svc_status = if *is_running { "running" } else { "stopped" };

        let _ = sqlx::query(
            "UPDATE project_services SET status = ? WHERE project_id = ? AND container_id = ? AND status != ?",
        )
        .bind(new_svc_status)
        .bind(project_id)
        .bind(container_id)
        .bind(new_svc_status)
        .execute(db)
        .await;
    }

    // Derive project status from aggregated service states
    let new_status = derive_and_set_project_status(db, project_id).await;
    let caddy_dirty = new_status != current_status;

    SyncResult {
        old_status: current_status,
        new_status,
        caddy_dirty,
    }
}

// ---------------------------------------------------------------------------
// Periodic reconciliation background task
// ---------------------------------------------------------------------------

/// Run periodic status sync every 60 seconds.
/// Corrects any drift between DB status and actual Docker container state.
pub async fn run_periodic_sync(state: crate::AppState) {
    let interval = std::time::Duration::from_secs(60);
    tracing::info!("periodic status sync started (interval: 60s)");

    loop {
        tokio::time::sleep(interval).await;

        let changed = sync_all_local_from_docker(&state.db, &state.docker).await;
        if !changed.is_empty() {
            tracing::info!(count = changed.len(), "periodic sync: corrected project statuses");
            let _ = state.route_sync_tx.send(());
        }
    }
}
