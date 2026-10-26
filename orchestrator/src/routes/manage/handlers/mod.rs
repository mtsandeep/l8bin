mod delete;
mod migrate;
mod recreate;
mod service_ops;
mod shared;
mod stop_start;

pub use delete::*;
pub use migrate::*;
pub use recreate::*;
pub use service_ops::*;
pub use stop_start::*;

/// Scoped volume names of a project (DB for multi-service, JSON for single).
pub(crate) async fn volume_list(db: &sqlx::SqlitePool, project: &crate::db::models::Project) -> Vec<String> {
    if shared::uses_compose_lifecycle(project.deploy_type.as_ref()) {
        sqlx::query_as::<_, (String,)>(
            "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(db)
        .await
        .map(|rows| rows.into_iter().map(|(name,)| name).collect())
        .unwrap_or_default()
    } else if let Some(ref vols_json) = project.volumes {
        serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(vols_json)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let name = v.name.as_deref().unwrap_or(&project.id);
                if name.starts_with('/') {
                    None
                } else {
                    Some(litebin_common::types::scope_volume_source(name, &project.id))
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}
