//! Demo mode — per-project public demos (docs/planning/demo-mode.md).
//!
//! Two exclusive modes: `shared` (one instance for all visitors, reset on an
//! interval) and `session` (a clone per visitor with a hard TTL). Instances
//! are real projects created by the copy engine, marked with
//! `projects.demo_parent_id` and hidden from the main grid. The parent is a
//! pure spec template — all visitor traffic is served by instances.

pub mod baseline;
pub mod clone;
pub mod gatekeeper;
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
    pub edit_instance_id: Option<String>,
    pub shared_instance_id: Option<String>,
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

async fn instance_exists(state: &AppState, parent_id: &str, instance_id: &str) -> bool {
    sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = ? AND demo_parent_id = ?")
        .bind(instance_id)
        .bind(parent_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0)
        > 0
}

/// Ensure the long-lived instances exist (and are recorded on the config):
/// the operator's edit instance always, the visitor-facing shared instance in
/// shared mode. Self-healing — safe to call from the janitor every tick.
pub async fn ensure_instances(state: &AppState, config: &mut DemoConfig, parent: &litebin_common::types::Project) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let baseline = baseline::load(state, &parent.id).await.ok();

    let edit_present = match config.edit_instance_id.as_deref() {
        Some(id) => instance_exists(state, &parent.id, id).await,
        None => false,
    };
    if !edit_present {
        let suffix: String = uuid::Uuid::new_v4().simple().to_string().chars().take(4).collect();
        let id = clone::create_instance(
            state,
            parent,
            clone::CloneOpts {
                start: true,
                baseline: baseline.clone(),
                requested_id: Some(format!("{}-edit-{suffix}", parent.id)),
            },
        )
        .await?;
        sqlx::query("UPDATE demo_config SET edit_instance_id = ?, updated_at = ? WHERE project_id = ?")
            .bind(&id)
            .bind(now)
            .bind(&parent.id)
            .execute(&state.db)
            .await?;
        config.edit_instance_id = Some(id);
        tracing::info!(project = %parent.id, "demo: edit instance created");
    }

    let want_shared = config.mode == "shared";
    let shared_present = match config.shared_instance_id.as_deref() {
        Some(id) => instance_exists(state, &parent.id, id).await,
        None => false,
    };
    if want_shared && !shared_present {
        let id = clone::create_instance(
            state,
            parent,
            clone::CloneOpts { start: true, baseline, requested_id: Some(format!("{}-shared", parent.id)) },
        )
        .await?;
        sqlx::query("UPDATE demo_config SET shared_instance_id = ?, updated_at = ? WHERE project_id = ?")
            .bind(&id)
            .bind(now)
            .bind(&parent.id)
            .execute(&state.db)
            .await?;
        config.shared_instance_id = Some(id);
        tracing::info!(project = %parent.id, "demo: shared instance created");
    }

    Ok(())
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
