use crate::AppState;

// ── Delete services ──────────────────────────────────────────────────────────

/// Remove all service containers, volumes, and the per-project network for a multi-service project.
/// Called from `delete_project`.
pub async fn delete_all_services(state: &AppState, project_id: &str) {
    // Fetch volume names from DB
    let volumes: Vec<String> = match sqlx::query_as::<_, (String,)>(
        "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v.into_iter().map(|(name,)| name).collect(),
        Err(e) => {
            tracing::warn!(project = %project_id, error = %e, "delete: failed to fetch volumes");
            Vec::new()
        }
    };

    let _ = state.docker.cleanup_project_resources(project_id, &volumes).await;
}
