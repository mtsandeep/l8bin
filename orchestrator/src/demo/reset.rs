//! Mode A reset — wipe the shared demo back to its baseline (or clean).
//!
//! Stop → remove volumes → restore baseline (if captured) → start. Container
//! recreation from the stored spec happens on start, exactly like duplicate.
//! The reset itself runs in the background; the demo host serves the themed
//! "resetting" page while the project status is `stopping`/`importing` (the
//! page ships with the gatekeeper — until then resets are simply fast).

use crate::copy_engine::{self, NodeRef};
use crate::demo;
use crate::AppState;
use litebin_common::types::{Project, ProjectStatus};

/// Trigger a background reset of a shared-mode demo. Returns immediately.
pub async fn reset_project(state: &AppState, parent_id: &str) -> anyhow::Result<()> {
    let parent: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(parent_id)
        .fetch_one(&state.db)
        .await?;
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
    let parent: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(parent_id)
        .fetch_one(&state.db)
        .await?;
    let node = demo::parent_node(&parent);

    // 1. Stop containers (files and volumes stay — stop_on_source semantics).
    copy_engine::stop_on_source(state, parent_id, &node).await?;

    // 2. Remove named volumes so stale files can't survive the reset.
    let volumes = crate::routes::manage::handlers::volume_list(&state.db, &parent).await;
    match &node {
        NodeRef::Local => {
            for volume in &volumes {
                if let Err(e) = state.docker.remove_volume(volume).await {
                    tracing::warn!(project = parent_id, volume = %volume, error = %e, "reset: volume removal skipped");
                }
            }
        }
        NodeRef::Agent(_) => {
            if let Ok(agent) = crate::nodes::client::AgentClient::resolve(state, node.node_id()).await {
                let _ = agent
                    .cleanup(&litebin_common::agent_api::CleanupRequest {
                        project_id: parent_id.to_string(),
                        volumes: volumes.clone(),
                    })
                    .await;
            }
        }
    }

    // 3. Restore the baseline if one is captured (seed → the parent's own
    //    scoped names); without a baseline the volumes come back empty at start.
    let baseline = demo::baseline::load(state, parent_id).await?;
    if !baseline.volumes.is_empty() || baseline.project_tar.is_some() {
        demo::baseline::restore(state, parent_id, parent_id, &baseline).await?;
    }

    // 4. Restart — recreates containers from the stored spec.
    match crate::routes::manage::handlers::start_project(
        axum::extract::State(state.clone()),
        axum::extract::Path(parent_id.to_string()),
    )
    .await
    {
        Ok(_) => tracing::info!(project = parent_id, "demo reset complete"),
        Err((code, message)) => anyhow::bail!("start after reset failed ({code}): {message}"),
    }
    Ok(())
}
