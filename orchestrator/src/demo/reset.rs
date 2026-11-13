//! Shared-mode reset — wipe the shared instance back to its baseline (or clean).
//!
//! Stop → remove volumes → restore baseline (if captured) → start; container
//! recreation from the stored spec happens on start, exactly like duplicate.
//! Runs in the background; visitors see the gatekeeper's pending page while
//! the shared instance restarts.

use crate::AppState;
use crate::copy_engine::{self, NodeRef};
use crate::demo;
use litebin_common::types::{Project, ProjectStatus};

/// Wipe one instance back to a clean boot (no baseline restore) — used by
/// "discard edits" so the edit copy doesn't resurrect discarded data on the
/// next publish.
pub async fn wipe_instance(state: &AppState, instance_id: &str) -> anyhow::Result<()> {
    let instance: Project =
        sqlx::query_as("SELECT * FROM projects WHERE id = ?").bind(instance_id).fetch_one(&state.db).await?;
    let node = demo::parent_node(&instance);

    copy_engine::stop_on_source(state, instance_id, &node).await?;
    let volumes = crate::routes::manage::handlers::volume_list(&state.db, &instance).await;
    if let NodeRef::Local = &node {
        for volume in &volumes {
            let _ = state.docker.remove_volume(volume).await;
        }
    }
    match crate::routes::manage::handlers::start_project(
        axum::extract::State(state.clone()),
        axum::extract::Path(instance_id.to_string()),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err((code, message)) => Err(anyhow::anyhow!("start after wipe failed ({code}): {message}")),
    }
}

/// Trigger a background reset of a shared-mode demo. Returns immediately.
pub async fn reset_project(state: &AppState, parent_id: &str) -> anyhow::Result<()> {
    let parent: Project =
        sqlx::query_as("SELECT * FROM projects WHERE id = ?").bind(parent_id).fetch_one(&state.db).await?;
    if demo::mid_operation(&parent.status) {
        anyhow::bail!("project is {} — wait for it to settle", parent.status);
    }

    let state_bg = state.clone();
    let id = parent_id.to_string();
    tokio::spawn(async move {
        if let Err(e) = run_reset(&state_bg, &id).await {
            tracing::error!(project = %id, error = %e, "demo reset failed");
            let _ = crate::status::transition(
                &state_bg.db,
                &id,
                ProjectStatus::Error,
                &crate::status::ProjectUpdateFields::default(),
                None,
            )
            .await;
        }
        let now = chrono::Utc::now().timestamp();
        let _ = sqlx::query("UPDATE demo_config SET last_reset_at = ?, updated_at = ? WHERE project_id = ?")
            .bind(now)
            .bind(now)
            .bind(&id)
            .execute(&state_bg.db)
            .await;
    });
    Ok(())
}

async fn run_reset(state: &AppState, parent_id: &str) -> anyhow::Result<()> {
    let parent: Project =
        sqlx::query_as("SELECT * FROM projects WHERE id = ?").bind(parent_id).fetch_one(&state.db).await?;

    // The reset wipes the visitor-facing shared instance (the parent is a
    // pure template; pre-provisioning configs fall back to the parent).
    let config = demo::get_config(&state.db, parent_id).await?;
    let target_id = config
        .as_ref()
        .and_then(|c| c.shared_instance_id.clone())
        .filter(|id| id != &parent.id)
        .unwrap_or_else(|| parent_id.to_string());
    let target: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(&target_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or_else(|_| parent.clone());
    let node = demo::parent_node(&target);

    // Surface the reset in progress on the waker's loading page (visitors of
    // a shared demo see "Stopping…" instead of a dead upstream).
    if matches!(target.status, ProjectStatus::Running | ProjectStatus::Degraded) {
        let _ = crate::status::transition(
            &state.db,
            &target_id,
            ProjectStatus::Stopping,
            &crate::status::ProjectUpdateFields::default(),
            None,
        )
        .await;
    }

    // 1. Stop containers (files and volumes stay — stop_on_source semantics).
    copy_engine::stop_on_source(state, &target_id, &node).await?;

    // 2. Remove named volumes so stale files can't survive the reset.
    let volumes = crate::routes::manage::handlers::volume_list(&state.db, &target).await;
    match &node {
        NodeRef::Local => {
            for volume in &volumes {
                if let Err(e) = state.docker.remove_volume(volume).await {
                    tracing::warn!(project = %target_id, volume = %volume, error = %e, "reset: volume removal skipped");
                }
            }
        }
        NodeRef::Agent(_) => {
            if let Ok(agent) = crate::nodes::client::AgentClient::resolve(state, node.node_id()).await {
                let _ = agent
                    .cleanup(&litebin_common::agent_api::CleanupRequest {
                        project_id: target_id.clone(),
                        volumes: volumes.clone(),
                    })
                    .await;
            }
        }
    }

    // 3. Restore the baseline if one is captured (seed volumes store
    //    parent-scoped names; restore re-scopes them to the target);
    //    without a baseline the volumes come back empty at start.
    let baseline = demo::baseline::load(state, parent_id).await?;
    if !baseline.volumes.is_empty() || baseline.project_tar.is_some() {
        demo::baseline::restore(state, parent_id, &target_id, &baseline).await?;
    }

    // 4. Restart — recreates containers from the stored spec.
    match crate::routes::manage::handlers::start_project(
        axum::extract::State(state.clone()),
        axum::extract::Path(target_id.clone()),
    )
    .await
    {
        Ok(_) => tracing::info!(project = %parent_id, instance = %target_id, "demo reset complete"),
        Err((code, message)) => anyhow::bail!("start after reset failed ({code}): {message}"),
    }
    Ok(())
}
