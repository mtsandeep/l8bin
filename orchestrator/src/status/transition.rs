use sqlx::SqlitePool;

use litebin_common::types::ProjectStatus;

use super::derive::refresh_oneshot_flags;

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
    new_status: ProjectStatus,
    extra: &ProjectUpdateFields,
    service_filter: Option<&[String]>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut tx = db.begin().await?;

    // 1. Update projects table using QueryBuilder for dynamic fields
    let mut qb = sqlx::QueryBuilder::new("UPDATE projects SET ");
    qb.push("status = ").push_bind(new_status.clone());
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
    match &new_status {
        ProjectStatus::Deploying | ProjectStatus::Importing | ProjectStatus::Running => {
            if let Some(services) = service_filter {
                for svc_name in services {
                    sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ? AND service_name = ?")
                        .bind(&new_status)
                        .bind(project_id)
                        .bind(svc_name)
                        .execute(&mut *tx)
                        .await?;
                }
            } else if new_status == ProjectStatus::Running {
                // Do not overwrite completed one-shot jobs when marking the project running
                sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ? AND is_oneshot = 0")
                    .bind(&new_status)
                    .bind(project_id)
                    .execute(&mut *tx)
                    .await?;
            } else {
                sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ?")
                    .bind(&new_status)
                    .bind(project_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        ProjectStatus::Stopped | ProjectStatus::Stopping | ProjectStatus::Error => {
            // Always cascade to ALL services for terminal/error states
            sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ?")
                .bind(&new_status)
                .bind(project_id)
                .execute(&mut *tx)
                .await?;
        }
        ProjectStatus::Degraded => {
            // Do NOT touch services — degraded is derived from individual service states
        }
        ProjectStatus::Completed => {
            // Service-only status; never set as a project-level status via transition
        }
        ProjectStatus::Pending | ProjectStatus::Unconfigured => {
            // Cascade setup states to any existing service rows
            sqlx::query("UPDATE project_services SET status = ? WHERE project_id = ?")
                .bind(&new_status)
                .bind(project_id)
                .execute(&mut *tx)
                .await?;
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
        "UPDATE project_services SET status = ?, container_id = ?, mapped_port = ? WHERE project_id = ? AND service_name = ?",
    )
    .bind(ProjectStatus::Running)
    .bind(container_id)
    .bind(mapped_port)
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark a one-shot service as completed after a successful exit (code 0).
pub async fn set_service_completed(
    db: &SqlitePool,
    project_id: &str,
    service_name: &str,
    container_id: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE project_services SET status = ?, container_id = ?, mapped_port = NULL, is_oneshot = 1 WHERE project_id = ? AND service_name = ?",
    )
    .bind(ProjectStatus::Completed)
    .bind(container_id)
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark a service as stopped, clearing its mapped_port.
/// Keeps container_id so the disk cache (keyed by container_id) remains valid.
/// Used by rollback, recreate cleanup, and partial redeploy.
pub async fn set_service_stopped(db: &SqlitePool, project_id: &str, service_name: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE project_services SET status = ?, mapped_port = NULL WHERE project_id = ? AND service_name = ?")
        .bind(ProjectStatus::Stopped)
        .bind(project_id)
        .bind(service_name)
        .execute(db)
        .await?;
    Ok(())
}

/// Mark a removed service container as stopped and clear runtime identifiers.
pub async fn set_service_removed(db: &SqlitePool, project_id: &str, service_name: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE project_services SET status = ?, container_id = NULL, mapped_port = NULL WHERE project_id = ? AND service_name = ?",
    )
    .bind(ProjectStatus::Stopped)
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark a service whose container was removed during a failed replacement.
/// Unlike a normal stop, both runtime identifiers are cleared.
pub async fn set_service_replacement_error(
    db: &SqlitePool,
    project_id: &str,
    service_name: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE project_services SET status = ?, container_id = NULL, mapped_port = NULL WHERE project_id = ? AND service_name = ?",
    )
    .bind(ProjectStatus::Error)
    .bind(project_id)
    .bind(service_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark only the project row as errored when a partial operation has already
/// assigned accurate per-service outcomes.
pub async fn set_project_error_only(db: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    let service_statuses: Vec<(String, ProjectStatus)> =
        sqlx::query_as("SELECT service_name, status FROM project_services WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&mut *tx)
            .await?;

    sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
        .bind(ProjectStatus::Error)
        .bind(chrono::Utc::now().timestamp())
        .bind(project_id)
        .execute(&mut *tx)
        .await?;

    // The database safety trigger deliberately cascades project errors. A
    // partial operation has already assigned precise service outcomes, so
    // restore those statuses in the same transaction before it is visible.
    for (service_name, service_status) in service_statuses {
        sqlx::query(
            "UPDATE project_services SET status = ?
             WHERE project_id = ? AND service_name = ?",
        )
        .bind(service_status)
        .bind(project_id)
        .bind(service_name)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Mark only the project row as stopping while preserving service metadata and
/// states until the identity-safe stop operation has definitely succeeded.
pub async fn set_project_stopping_only(db: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    set_project_status_only(db, project_id, ProjectStatus::Stopping).await
}

pub async fn set_project_stopped_only(db: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    set_project_status_only(db, project_id, ProjectStatus::Stopped).await
}

/// Persist a successful identity-safe full stop without changing completed
/// one-shot rows or clearing cached container IDs.
pub async fn set_non_oneshot_services_stopped(db: &SqlitePool, project_id: &str) -> anyhow::Result<()> {
    refresh_oneshot_flags(db, project_id).await;
    sqlx::query("UPDATE project_services SET status = ?, mapped_port = NULL WHERE project_id = ? AND is_oneshot = 0")
        .bind(ProjectStatus::Stopped)
        .bind(project_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn set_project_status_only(
    db: &SqlitePool,
    project_id: &str,
    project_status: ProjectStatus,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
        .bind(project_status)
        .bind(chrono::Utc::now().timestamp())
        .bind(project_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Update the single-service "web" row in project_services to match projects table.
/// Single-service projects have a "web" row that must track container_id/mapped_port.
/// Called by waker and handlers after creating or starting a single-service container.
pub async fn sync_single_service_row(db: &SqlitePool, project_id: &str, container_id: &str, mapped_port: i64) {
    if let Err(e) = sqlx::query(
        "UPDATE project_services SET status = ?, container_id = ?, mapped_port = ? WHERE project_id = ? AND service_name = 'web'",
    )
    .bind(ProjectStatus::Running)
    .bind(container_id)
    .bind(mapped_port)
    .bind(project_id)
    .execute(db)
    .await
    {
        tracing::warn!(project_id = %project_id, error = %e, "status: failed to sync single service row");
    }
}
