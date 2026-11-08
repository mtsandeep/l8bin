//! Demo baseline — a frozen seed of the demo's starting data.
//!
//! Named volumes are captured as seed volumes on the parent's node
//! (`l8b_demo_seed_{parent}_{n}` — the `l8b_` prefix can never collide with
//! the `litebin_{project}_` scope). The project dir (compose.yaml, .env,
//! relative binds) is captured as a tar under `data/demo-seeds/{id}/`.
//! Capture briefly stops its source for a consistent snapshot; instances and
//! resets restore from the frozen copy and never touch the running source.

use std::path::PathBuf;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::copy_engine::{self, NodeRef};
use crate::demo::parent_node;
use crate::AppState;

/// A baseline row set: seed volumes + the project-dir tar path (if captured).
#[derive(Clone)]
pub struct Baseline {
    pub volumes: Vec<(String, String)>, // (source_volume, seed_volume)
    pub project_tar: Option<PathBuf>,
}

/// The demo-seeds folder under the orchestrator data dir.
fn seeds_dir(state: &AppState) -> PathBuf {
    crate::backup::resolve_data_dir(&state.config.database_url)
        .unwrap_or_else(|| PathBuf::from("data"))
        .join("demo-seeds")
}

/// Load the captured baseline for a parent; `volumes` empty + no tar = none.
pub async fn load(state: &AppState, parent_id: &str) -> anyhow::Result<Baseline> {
    let volumes: Vec<(String, String)> = sqlx::query_as(
        "SELECT source_volume, seed_volume FROM demo_baseline_volumes WHERE parent_project_id = ?",
    )
    .bind(parent_id)
    .fetch_all(&state.db)
    .await?;
    let tar = seeds_dir(state).join(parent_id).join("project.tar");
    let project_tar = tokio::fs::metadata(&tar).await.is_ok().then_some(tar);
    Ok(Baseline { volumes, project_tar })
}

/// Capture the demo's starting data as the baseline: the first capture from
/// the PARENT (the deployed, configured app — instances start volume-empty),
/// recaptures from the operator's edit instance. Briefly stops the source
/// for a consistent snapshot, then restarts it. Re-capture overwrites the
/// previous seed.
pub async fn capture(state: &AppState, parent: &litebin_common::types::Project) -> anyhow::Result<()> {
    let config = crate::demo::get_config(&state.db, &parent.id).await?;
    // First capture seeds from the PARENT (the deployed + prepped app — the
    // only good copy; instances start volume-empty, so capturing from the
    // edit instance then would freeze blank data). Recaptures come from the
    // edit instance where the operator makes changes.
    let source: litebin_common::types::Project = match (
        config.as_ref().and_then(|c| c.edit_instance_id.clone()),
        config.as_ref().map(|c| c.baseline_captured_at.is_some()),
    ) {
        (Some(id), Some(true)) => sqlx::query_as("SELECT * FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| parent.clone()),
        _ => parent.clone(),
    };
    let node = parent_node(&source);
    let was_running = matches!(
        source.status,
        litebin_common::types::ProjectStatus::Running | litebin_common::types::ProjectStatus::Degraded
    );

    if was_running {
        copy_engine::stop_on_source(state, &source.id, &node).await?;
    }

    let result = capture_locked(state, parent, &source, &node).await;

    if was_running {
        let _ = crate::routes::manage::handlers::start_project(
            axum::extract::State(state.clone()),
            axum::extract::Path(source.id.clone()),
        )
        .await;
    }

    result?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE demo_config SET baseline_captured_at = ?, updated_at = ? WHERE project_id = ?")
        .bind(now)
        .bind(now)
        .bind(&parent.id)
        .execute(&state.db)
        .await?;
    tracing::info!(project = %parent.id, source = %source.id, "demo baseline captured");
    Ok(())
}

/// Snapshot while the source is stopped. Wipes the previous seed first so a
/// failed capture never leaves a half-old half-new baseline. Volume names and
/// the project-dir tar are stored PARENT-scoped so restores re-scope cleanly
/// to any target.
async fn capture_locked(
    state: &AppState,
    parent: &litebin_common::types::Project,
    source: &litebin_common::types::Project,
    node: &NodeRef,
) -> anyhow::Result<()> {
    remove_seed_volumes(state, &parent.id, node).await?;
    sqlx::query("DELETE FROM demo_baseline_volumes WHERE parent_project_id = ?")
        .bind(&parent.id)
        .execute(&state.db)
        .await?;

    let volumes = crate::routes::manage::handlers::volume_list(&state.db, source).await;
    for (i, scoped) in volumes.iter().enumerate() {
        let seed = seed_name(&parent.id, i);
        copy_engine::transfer_volume(state, node, node, scoped, &seed).await?;
        let parent_scoped = if source.id == parent.id {
            scoped.clone()
        } else {
            copy_engine::rescope_volume(scoped, &source.id, &parent.id)
        };
        sqlx::query(
            "INSERT INTO demo_baseline_volumes (parent_project_id, source_volume, seed_volume) VALUES (?, ?, ?)",
        )
        .bind(&parent.id)
        .bind(&parent_scoped)
        .bind(&seed)
        .execute(&state.db)
        .await?;
    }

    // Project dir: compose.yaml/.env/relative binds, stored parent-rooted.
    // Absent for single-service image projects that never wrote files — a
    // missing export means no seed.
    let dir = seeds_dir(state).join(&parent.id);
    tokio::fs::create_dir_all(&dir).await?;
    let tar_path = dir.join("project.tar");
    match node.transfer_out(state, &litebin_common::docker::TransferKind::ProjectDir(source.id.clone())).await {
        Ok(stream) => {
            let tar_stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = bytes::Bytes> + Send>> =
                if source.id == parent.id {
                    stream
                } else {
                    let tmp = copy_engine::spool_dir(state);
                    Box::pin(copy_engine::reroot::reroot(stream, &source.id, &parent.id, true, &tmp).await?)
                };
            let mut file = tokio::fs::File::create(&tar_path).await?;
            let mut tar_stream = tar_stream;
            while let Some(chunk) = tar_stream.next().await {
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
        }
        Err(e) => {
            // No project dir to seed — drop any stale tar and record volumes-only.
            tracing::debug!(project = %parent.id, error = %e, "demo baseline: no project dir captured");
            let _ = tokio::fs::remove_file(&tar_path).await;
        }
    }
    Ok(())
}

/// Remove the baseline: seed volumes, seed tar, and DB rows.
pub async fn remove(state: &AppState, parent: &litebin_common::types::Project) -> anyhow::Result<()> {
    remove_seed_volumes(state, &parent.id, &parent_node(parent)).await?;
    sqlx::query("DELETE FROM demo_baseline_volumes WHERE parent_project_id = ?")
        .bind(&parent.id)
        .execute(&state.db)
        .await?;
    let dir = seeds_dir(state).join(&parent.id);
    let _ = tokio::fs::remove_dir_all(&dir).await;
    sqlx::query("UPDATE demo_config SET baseline_captured_at = NULL, updated_at = ? WHERE project_id = ?")
        .bind(chrono::Utc::now().timestamp())
        .bind(&parent.id)
        .execute(&state.db)
        .await?;
    tracing::info!(project = %parent.id, "demo baseline removed");
    Ok(())
}

/// Restore the baseline into project id `new_id` (the shared instance on
/// reset, a fresh instance on clone).
/// Volumes copy seed → scoped target; the project-dir tar re-roots
/// `{parent}/…` → `{new_id}/…` (binds inside ride along).
pub async fn restore(state: &AppState, parent_id: &str, new_id: &str, baseline: &Baseline) -> anyhow::Result<()> {
    let parent: litebin_common::types::Project =
        sqlx::query_as("SELECT * FROM projects WHERE id = ?").bind(parent_id).fetch_one(&state.db).await?;
    let node = parent_node(&parent);

    for (source, seed) in &baseline.volumes {
        let target = copy_engine::rescope_volume(source, parent_id, new_id);
        copy_engine::transfer_volume(state, &node, &node, seed, &target).await?;
    }

    if let Some(tar) = &baseline.project_tar {
        let file = tokio::fs::File::open(tar).await?;
        let raw = tokio_util::io::ReaderStream::new(file).map(|c| c.unwrap_or_default());
        let tar_stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = bytes::Bytes> + Send>> =
            if parent_id == new_id {
                Box::pin(raw)
            } else {
                let tmp = copy_engine::spool_dir(state);
                Box::pin(copy_engine::reroot::reroot(
                    Box::pin(raw),
                    parent_id,
                    new_id,
                    true,
                    &tmp,
                ).await?)
            };
        node.transfer_in(
            state,
            &litebin_common::docker::TransferKind::ProjectDir(new_id.to_string()),
            tar_stream,
        )
        .await?;
    }
    Ok(())
}

fn seed_name(parent_id: &str, index: usize) -> String {
    // `l8b_` never matches the `litebin_{project}_` rescope prefix.
    format!("l8b_demo_seed_{parent_id}_{index}")
}

/// Delete the parent's seed volumes on its node. Local removes them outright;
/// agents have no volume-delete endpoint, so their seeds are zeroed via an
/// empty-tar import (an empty volume lingers until the node prunes it).
async fn remove_seed_volumes(state: &AppState, parent_id: &str, node: &NodeRef) -> anyhow::Result<()> {
    let seeds: Vec<String> =
        sqlx::query_scalar("SELECT seed_volume FROM demo_baseline_volumes WHERE parent_project_id = ?")
            .bind(parent_id)
            .fetch_all(&state.db)
            .await?;
    for seed in seeds {
        match node {
            NodeRef::Local => state.docker.remove_volume(&seed).await?,
            NodeRef::Agent(_) => {
                let empty: std::pin::Pin<Box<dyn futures_util::Stream<Item = bytes::Bytes> + Send>> =
                    Box::pin(futures_util::stream::empty());
                let _ = node
                    .transfer_in(state, &litebin_common::docker::TransferKind::Volume(seed), empty)
                    .await;
            }
        }
    }
    Ok(())
}
