//! Demo mode — per-project public demos (docs/planning/demo-mode.md).
//!
//! Two exclusive modes: `shared` (one instance for all visitors, reset on an
//! interval) and `session` (a clone per visitor with a hard TTL). Instances
//! are real projects created by the copy engine, marked with
//! `projects.demo_parent_id` and hidden from the main grid. The base project
//! is the template spec — it never serves visitor traffic in session mode.

pub mod baseline;
pub mod clone;
pub mod janitor;
pub mod reset;

use sqlx::FromRow;

use crate::copy_engine::NodeRef;
use crate::AppState;

#[derive(Debug, Clone, serde::Serialize, FromRow, utoipa::ToSchema)]
pub struct DemoConfig {
    pub project_id: String,
    pub mode: String,
    pub max_instances: i64,
    pub session_ttl_minutes: i64,
    pub prewarm: i64,
    pub reset_interval_minutes: Option<i64>,
    pub banner_text: String,
    pub bar_enabled: bool,
    pub last_reset_at: Option<i64>,
    pub baseline_captured_at: Option<i64>,
    pub updated_at: i64,
}

/// Load a project's demo config; None when demo mode is off.
pub async fn get_config(db: &sqlx::SqlitePool, project_id: &str) -> anyhow::Result<Option<DemoConfig>> {
    Ok(sqlx::query_as::<_, DemoConfig>("SELECT * FROM demo_config WHERE project_id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await?)
}

/// The parent's node as a copy-engine NodeRef.
pub fn parent_node(project: &litebin_common::types::Project) -> NodeRef {
    match project.node_id.as_deref() {
        Some(id) if id != "local" => NodeRef::Agent(id.to_string()),
        _ => NodeRef::Local,
    }
}

/// Whether the project is in a state a reset/clone must not interrupt.
pub fn mid_operation(status: &litebin_common::types::ProjectStatus) -> bool {
    use litebin_common::types::ProjectStatus as S;
    matches!(
        status,
        S::Deploying | S::Migrating | S::Exporting | S::Stopping | S::Importing
    )
}

/// Delete a project's DB row and its node resources via the existing delete
/// path (containers, volumes, networks, routes, images). Callable directly,
/// like `start_project`.
pub async fn teardown_instance(state: &AppState, project_id: &str) {
    if let Err((code, message)) =
        crate::routes::manage::handlers::delete_project(
            axum::extract::State(state.clone()),
            axum::extract::Path(project_id.to_string()),
        )
        .await
    {
        tracing::warn!(project = project_id, status = %code, error = %message, "demo: instance teardown failed");
    }
}
