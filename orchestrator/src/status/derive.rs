use sqlx::SqlitePool;

use litebin_common::types::ProjectStatus;

/// Derive project status from aggregated service states and update projects table.
/// Returns the derived status.
///
/// - All long-running services running and one-shots completed → Running
/// - Some long-running services running → Degraded
/// - No long-running services running → Stopped
pub async fn derive_and_set_project_status(db: &SqlitePool, project_id: &str) -> ProjectStatus {
    // Refresh is_oneshot from stored depends_on JSON (covers projects deployed before the column existed)
    refresh_oneshot_flags(db, project_id).await;

    let rows: Vec<(ProjectStatus, bool)> = match sqlx::query_as(
        "SELECT status, is_oneshot FROM project_services WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "status: failed to fetch service statuses, defaulting to Stopped");
            return ProjectStatus::Stopped;
        }
    };

    if rows.is_empty() {
        return ProjectStatus::Stopped;
    }

    let mut healthy = 0usize;
    let mut long_running_up = 0usize;
    let mut long_running_total = 0usize;
    for (status, is_oneshot) in &rows {
        if status.is_service_healthy() {
            healthy += 1;
        }
        if !*is_oneshot {
            long_running_total += 1;
            if *status == ProjectStatus::Running {
                long_running_up += 1;
            }
        }
    }

    let new_status = if healthy == rows.len() {
        ProjectStatus::Running
    } else if long_running_up > 0 {
        ProjectStatus::Degraded
    } else if long_running_total == 0 && healthy > 0 {
        // Only one-shots present and some completed — treat as stopped until daemons exist
        ProjectStatus::Stopped
    } else {
        ProjectStatus::Stopped
    };

    let now = chrono::Utc::now().timestamp();
    if let Err(e) = sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
        .bind(&new_status)
        .bind(now)
        .bind(project_id)
        .execute(db)
        .await
    {
        tracing::warn!(project_id = %project_id, status = %new_status, error = %e, "status: failed to update project status");
    }

    new_status
}

/// Parse `depends_on` JSON stored on service rows and set `is_oneshot` for
/// services referenced with `service_completed_successfully`.
pub(super) async fn refresh_oneshot_flags(db: &SqlitePool, project_id: &str) {
    let deps: Vec<(String, Option<String>)> =
        match sqlx::query_as("SELECT service_name, depends_on FROM project_services WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(db)
            .await
        {
            Ok(r) => r,
            Err(_) => return,
        };

    let mut oneshots = std::collections::HashSet::new();
    for (_name, depends_on) in &deps {
        let Some(raw) = depends_on else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        match value {
            serde_json::Value::Object(map) => {
                for (dep, spec) in map {
                    let cond = spec.get("condition").and_then(|c| c.as_str()).unwrap_or("service_started");
                    if cond == "service_completed_successfully" {
                        oneshots.insert(dep);
                    }
                }
            }
            _ => {}
        }
    }

    if oneshots.is_empty() {
        return;
    }

    for name in &oneshots {
        if let Err(e) =
            sqlx::query("UPDATE project_services SET is_oneshot = 1 WHERE project_id = ? AND service_name = ?")
                .bind(project_id)
                .bind(name)
                .execute(db)
                .await
        {
            tracing::warn!(project_id = %project_id, service = %name, error = %e, "status: failed to set is_oneshot");
        }
    }
}
