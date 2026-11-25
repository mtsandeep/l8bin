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

use crate::AppState;
use crate::copy_engine::{self, NodeRef};
use crate::demo::parent_node;
use litebin_common::types::{Project, ProjectStatus};

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
    let volumes: Vec<(String, String)> =
        sqlx::query_as("SELECT source_volume, seed_volume FROM demo_baseline_volumes WHERE parent_project_id = ?")
            .bind(parent_id)
            .fetch_all(&state.db)
            .await?;
    let tar = seeds_dir(state).join(parent_id).join("project.tar");
    // An empty tar is a source with no project dir, not a seed to restore from.
    let project_tar = tokio::fs::metadata(&tar).await.is_ok_and(|meta| meta.len() > 0).then_some(tar);
    Ok(Baseline { volumes, project_tar })
}

/// Capture the demo's starting data as the baseline: the first capture from
/// the PARENT (the deployed, configured app — instances start volume-empty),
/// recaptures from the operator's edit instance. Briefly stops the source
/// for a consistent snapshot, then restarts it. Re-capture replaces the
/// previous seed.
pub async fn capture(state: &AppState, parent: &Project) -> anyhow::Result<()> {
    let config = crate::demo::get_config(&state.db, &parent.id).await?;
    // Source: the edit instance (the operator's draft) whenever it exists;
    // the parent only before instances are provisioned (the enable flow
    // captures before creating them).
    let source: Project = match config.as_ref().and_then(|c| c.edit_instance_id.clone()) {
        Some(id) => sqlx::query_as("SELECT * FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| parent.clone()),
        None => parent.clone(),
    };
    let node = parent_node(&source);
    let was_running = matches!(source.status, ProjectStatus::Running | ProjectStatus::Degraded);

    // Not while a clone or reset is copying from the seed.
    let seed_lock = crate::demo::seed_lock(&parent.id).await;
    let _seed_guard = seed_lock.lock().await;

    // Stopped whatever the status says: a stale row with live containers would give a hot snapshot.
    let source_hold = copy_engine::stop_on_source(state, &source.id, &node).await?;
    let result = capture_locked(state, parent, &source, &node).await;
    let stopped_any = source_hold.stopped_any;
    // Before the restart: `start_project` takes the same operation lock.
    drop(source_hold);

    if was_running || stopped_any {
        let restart = crate::routes::manage::handlers::start_project(
            axum::extract::State(state.clone()),
            axum::extract::Path(source.id.clone()),
        )
        .await;
        if let Err((code, message)) = restart {
            tracing::warn!(project = %source.id, status = %code, error = %message, "demo baseline: source restart failed");
        }
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

/// Snapshot while the source is stopped. The previous baseline is retired
/// first and the new one published last, so a failed capture leaves no
/// baseline at all. Volume names and the tar are stored PARENT-scoped.
async fn capture_locked(state: &AppState, parent: &Project, source: &Project, node: &NodeRef) -> anyhow::Result<()> {
    // 1. Retire the previous baseline.
    let mut stale_seeds = seed_names(state, parent).await?;
    sqlx::query("DELETE FROM demo_baseline_volumes WHERE parent_project_id = ?")
        .bind(&parent.id)
        .execute(&state.db)
        .await?;
    let dir = seeds_dir(state).join(&parent.id);
    let _ = tokio::fs::remove_dir_all(&dir).await;

    let volumes = crate::routes::manage::handlers::volume_list(&state.db, source).await;
    // One seed per distinct volume — a volume mounted by several services is listed once per service.
    let mut seen = std::collections::HashSet::new();
    let volumes: Vec<String> = volumes.into_iter().filter(|v| seen.insert(v.clone())).collect();
    for i in 0..volumes.len() {
        let seed = seed_name(&parent.id, i);
        if !stale_seeds.contains(&seed) {
            stale_seeds.push(seed);
        }
    }

    // 2. Remove the old seeds outright: a tar import only adds files.
    for seed in &stale_seeds {
        remove_seed(state, node, seed).await?;
    }

    // 3. Each volume into a fresh seed.
    let mut rows: Vec<(String, String)> = Vec::new(); // (parent-scoped source, seed)
    for (i, scoped) in volumes.iter().enumerate() {
        let seed = seed_name(&parent.id, i);
        copy_engine::transfer_volume(state, node, node, scoped, &seed).await?;
        let parent_scoped = if source.id == parent.id {
            scoped.clone()
        } else {
            copy_engine::rescope_volume(scoped, &source.id, &parent.id)
        };
        rows.push((parent_scoped, seed));
    }

    // 4. Project dir: compose.yaml/.env/relative binds, stored parent-rooted.
    // Absent for single-service image projects that never wrote files — a
    // missing export means no seed.
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
            // Renamed into place so a failed write leaves no half-written tar.
            tokio::fs::create_dir_all(&dir).await?;
            let partial_path = dir.join("project.tar.part");
            let written = async {
                let mut file = tokio::fs::File::create(&partial_path).await?;
                let mut tar_stream = tar_stream;
                while let Some(chunk) = tar_stream.next().await {
                    file.write_all(&chunk).await?;
                }
                file.flush().await?;
                let len = tokio::fs::metadata(&partial_path).await?.len();
                anyhow::Ok(len)
            }
            .await;
            let len = match written {
                Ok(len) => len,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&partial_path).await;
                    return Err(e);
                }
            };
            // A local export of a missing dir is an empty stream, not an error.
            if len == 0 {
                let _ = tokio::fs::remove_file(&partial_path).await;
                tracing::debug!(project = %parent.id, "demo baseline: no project dir captured");
            } else {
                tokio::fs::rename(&partial_path, &tar_path).await?;
            }
        }
        Err(e) => {
            // No project dir to seed — record volumes-only.
            tracing::debug!(project = %parent.id, error = %e, "demo baseline: no project dir captured");
            let _ = tokio::fs::remove_file(&tar_path).await;
        }
    }

    // 5. Publish, all rows at once.
    let mut tx = state.db.begin().await?;
    for (source_volume, seed) in rows {
        sqlx::query(
            "INSERT INTO demo_baseline_volumes (parent_project_id, source_volume, seed_volume) VALUES (?, ?, ?)",
        )
        .bind(&parent.id)
        .bind(source_volume)
        .bind(seed)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Remove the baseline: seed volumes, seed tar, and DB rows.
pub async fn remove(state: &AppState, parent: &Project) -> anyhow::Result<()> {
    let seed_lock = crate::demo::seed_lock(&parent.id).await;
    let _seed_guard = seed_lock.lock().await;
    remove_seed_volumes(state, parent, &parent_node(parent)).await?;
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
    let parent: Project =
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
                Box::pin(copy_engine::reroot::reroot(Box::pin(raw), parent_id, new_id, true, &tmp).await?)
            };
        node.transfer_in(state, &litebin_common::docker::TransferKind::ProjectDir(new_id.to_string()), tar_stream)
            .await?;
    }
    Ok(())
}

fn seed_name(parent_id: &str, index: usize) -> String {
    // `l8b_` never matches the `litebin_{project}_` rescope prefix.
    format!("l8b_demo_seed_{parent_id}_{index}")
}

/// Every seed the parent may own: the recorded ones plus one per current
/// volume (a capture that fails before publishing leaves those without a row).
async fn seed_names(state: &AppState, parent: &Project) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> =
        sqlx::query_scalar("SELECT seed_volume FROM demo_baseline_volumes WHERE parent_project_id = ?")
            .bind(&parent.id)
            .fetch_all(&state.db)
            .await?;
    for i in 0..crate::routes::manage::handlers::volume_list(&state.db, parent).await.len() {
        let seed = seed_name(&parent.id, i);
        if !names.contains(&seed) {
            names.push(seed);
        }
    }
    Ok(names)
}

/// Delete the parent's seed volumes on its node.
async fn remove_seed_volumes(state: &AppState, parent: &Project, node: &NodeRef) -> anyhow::Result<()> {
    for seed in seed_names(state, parent).await? {
        remove_seed(state, node, &seed).await?;
    }
    Ok(())
}

/// Remove one seed volume outright — the next transfer recreates it empty.
async fn remove_seed(state: &AppState, node: &NodeRef, seed: &str) -> anyhow::Result<()> {
    match node {
        NodeRef::Local => state.docker.remove_volume(seed).await,
        NodeRef::Agent(node_id) => {
            let agent = crate::nodes::client::AgentClient::resolve(state, node_id).await?;
            agent.remove_volume(seed).await.map_err(|e| anyhow::anyhow!("seed removal failed: {e}"))
        }
    }
}
