//! Shared copy engine — project payload movement with optional project-id rename.
//!
//! Migration moves a project under the same id (handlers/migrate.rs owns that
//! flow); duplication copies a project under a NEW id without touching the
//! source; export/import round-trip through a tar.gz bundle. All ride the same
//! `TransferKind` tar streams. The rename cascade (containers, network, volume
//! scope, subdomain) needs no content rewriting — every unique name derives
//! from the project id at run time.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use bytes::Bytes;
use futures_util::Stream;
use litebin_common::docker::TransferKind;
use litebin_common::types::scope_volume_source;

use crate::AppState;

pub mod bundle;
pub mod ids;
pub mod manifest;
pub mod reroot;

pub use ids::allocate_project_id;

/// Image refs a project deploys from: service rows for compose projects,
/// the primary image otherwise. Uploaded (`sha256:`) refs are the ones that
/// must transfer; registry refs pull fresh on the target.
pub async fn project_image_refs(db: &sqlx::SqlitePool, project: &litebin_common::types::Project) -> Vec<String> {
    if project.deploy_type == Some(litebin_common::types::DeployType::Compose) {
        sqlx::query_scalar("SELECT image FROM project_services WHERE project_id = ? AND image IS NOT NULL")
            .bind(&project.id)
            .fetch_all(db)
            .await
            .unwrap_or_default()
    } else {
        project.image.clone().into_iter().collect()
    }
}

/// Stop a project on its node so volume tars are consistent — used by export
/// before snapshotting. Containers stop; files and volumes always stay.
pub async fn stop_on_source(state: &AppState, project_id: &str, node: &NodeRef) -> anyhow::Result<()> {
    match node {
        NodeRef::Local => {
            let _ = state.docker.remove_by_name(project_id).await;
            crate::routes::manage::multi_service::stop_services(state, project_id, None).await.ok();
            Ok(())
        }
        NodeRef::Agent(node_id) => {
            let agent = crate::nodes::client::AgentClient::resolve(state, node_id).await?;
            agent
                .stop_project(&litebin_common::agent_api::StopProjectRequest { project_id: project_id.to_string() })
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("source stop failed: {e}"))
        }
    }
}

/// The exports folder (export bundles land here; host-bind mounted in prod).
pub fn exports_dir() -> PathBuf {
    if let Ok(v) = std::env::var("EXPORTS_DIR") {
        return PathBuf::from(v);
    }
    data_dir().join("exports")
}

/// The imports folder (user-scp'd bundles are imported from here).
pub fn imports_dir() -> PathBuf {
    if let Ok(v) = std::env::var("IMPORTS_DIR") {
        return PathBuf::from(v);
    }
    data_dir().join("imports")
}

fn data_dir() -> PathBuf {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:./data/litebin.db".into());
    crate::backup::resolve_data_dir(&url).unwrap_or_else(|| std::path::PathBuf::from("data"))
}

/// A source or target for project payloads: the orchestrator's own Docker
/// (`Local`) or a remote agent node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRef {
    Local,
    Agent(String),
}

impl NodeRef {
    /// `"local"` for the orchestrator's own Docker, the node id otherwise.
    pub fn node_id(&self) -> &str {
        match self {
            NodeRef::Local => "local",
            NodeRef::Agent(id) => id,
        }
    }

    pub async fn transfer_out(
        &self,
        state: &AppState,
        kind: &TransferKind,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Bytes> + Send>>> {
        use futures_util::StreamExt;
        match self {
            NodeRef::Local => {
                let projects_dir = state.docker.projects_host_dir();
                let kind = local_volume_kind(state, kind);
                let s = state.docker.transfer_export(&kind, projects_dir).await?;
                Ok(Box::pin(s.map(|chunk| chunk.unwrap_or_default())))
            }
            NodeRef::Agent(node_id) => {
                let agent = crate::nodes::client::AgentClient::resolve(state, node_id).await?;
                let s = agent.transfer_export_stream(kind).await?;
                Ok(Box::pin(s.map(|chunk| chunk.unwrap_or_default())))
            }
        }
    }

    pub async fn transfer_in(
        &self,
        state: &AppState,
        kind: &TransferKind,
        tar: Pin<Box<dyn Stream<Item = Bytes> + Send>>,
    ) -> anyhow::Result<()> {
        match self {
            NodeRef::Local => {
                let projects_dir = state.docker.projects_host_dir();
                let kind = local_volume_kind(state, kind);
                state.docker.transfer_import(&kind, projects_dir, tar).await
            }
            NodeRef::Agent(node_id) => {
                let agent = crate::nodes::client::AgentClient::resolve(state, node_id).await?;
                agent.transfer_import(kind, tar).await.map_err(|e| anyhow::anyhow!("{e}"))
            }
        }
    }

    /// Node architecture from the `nodes` row (the heartbeat keeps the `local`
    /// row fresh); None when unreported.
    pub async fn architecture(&self, state: &AppState) -> Option<String> {
        sqlx::query_scalar("SELECT architecture FROM nodes WHERE id = ?")
            .bind(self.node_id())
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .flatten()
    }
}

/// Copy one Docker volume. The target name may differ — both the local
/// manager and the agent accept a free target name, so renames are free.
pub async fn transfer_volume(
    state: &AppState,
    src: &NodeRef,
    dst: &NodeRef,
    from_name: &str,
    to_name: &str,
) -> anyhow::Result<()> {
    tracing::info!(%from_name, %to_name, src = src.node_id(), dst = dst.node_id(), "transferring volume");
    let stream = src.transfer_out(state, &TransferKind::Volume(from_name.to_string())).await?;
    dst.transfer_in(state, &TransferKind::Volume(to_name.to_string()), stream).await
}

/// Copy a project dir. When the id changes (duplicate/import) the tar's
/// top-level `{old_id}/` directory is re-rooted to `{new_id}/` and `.env*` is
/// optionally filtered (`.env.l8bin` snapshots are always dropped — they are
/// regenerated at start). `tmp_dir` spools the rewrite.
pub async fn transfer_project_dir(
    state: &AppState,
    src: &NodeRef,
    dst: &NodeRef,
    old_id: &str,
    new_id: &str,
    include_env: bool,
    tmp_dir: &Path,
) -> anyhow::Result<()> {
    tracing::info!(old_id, new_id, src = src.node_id(), dst = dst.node_id(), "transferring project dir");
    let raw = src.transfer_out(state, &TransferKind::ProjectDir(old_id.to_string())).await?;
    let tar: Pin<Box<dyn Stream<Item = Bytes> + Send>> = if old_id == new_id && include_env {
        raw
    } else {
        reroot::reroot(raw, old_id, new_id, include_env, tmp_dir).await?
    };
    dst.transfer_in(state, &TransferKind::ProjectDir(new_id.to_string()), tar).await
}

/// Copy an uploaded (`sha256:`) image via docker save/load. Registry refs are
/// never moved — the target pulls them fresh.
pub async fn transfer_image(state: &AppState, src: &NodeRef, dst: &NodeRef, image: &str) -> anyhow::Result<()> {
    tracing::info!(image, src = src.node_id(), dst = dst.node_id(), "transferring image");
    let stream = src.transfer_out(state, &TransferKind::Image(image.to_string())).await?;
    dst.transfer_in(state, &TransferKind::Image(image.to_string()), stream).await
}

/// Relative binds (`projects/…` scoped names) must be mounted by HOST path —
/// Docker rejects them as volume names. Named volumes and absolute binds pass
/// through unchanged. Local-node transfers only; agents resolve on their side.
pub(crate) fn local_volume_kind(state: &AppState, kind: &TransferKind) -> TransferKind {
    match kind {
        TransferKind::Volume(v) if v.starts_with("projects/") => {
            TransferKind::Volume(resolve_scoped_volume(v, state.docker.projects_host_dir()))
        }
        _ => kind.clone(),
    }
}

fn resolve_scoped_volume(scoped: &str, host_projects_dir: Option<&str>) -> String {
    match (scoped.strip_prefix("projects/"), host_projects_dir) {
        (Some(rest), Some(host)) => format!("{host}/{rest}"),
        _ => scoped.to_string(),
    }
}

/// Re-scope a scoped volume/bind name from one project id to another:/// `litebin_{old}_{name}` → `litebin_{new}_{name}`, `projects/{old}/…` →
/// `projects/{new}/…`. Absolute binds and unknown shapes pass through.
pub fn rescope_volume(scoped: &str, old_id: &str, new_id: &str) -> String {
    if let Some(rest) = scoped.strip_prefix(&format!("litebin_{old_id}_")) {
        scope_volume_source(rest, new_id)
    } else if let Some(rest) = scoped.strip_prefix(&format!("projects/{old_id}/")) {
        format!("projects/{new_id}/{rest}")
    } else {
        scoped.to_string()
    }
}

/// Directory for copy-engine spool files. Prefers the orchestrator data dir
/// (resolved from DATABASE_URL); falls back to the system temp dir for
/// in-memory test databases.
pub fn spool_dir(state: &AppState) -> PathBuf {
    crate::backup::resolve_data_dir(&state.config.database_url)
        .map(|d| d.join("tmp"))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::{rescope_volume, resolve_scoped_volume};

    #[test]
    fn rescopes_named_volumes_and_relative_binds() {
        assert_eq!(rescope_volume("litebin_old_pgdata", "old", "new"), "litebin_new_pgdata");
        assert_eq!(rescope_volume("projects/old/data", "old", "new"), "projects/new/data");
        // Default-name single-service volumes (scoped with the project id itself).
        assert_eq!(rescope_volume("litebin_old_old", "old", "new"), "litebin_new_old");
    }

    #[test]
    fn passes_through_absolute_binds_and_unknown_shapes() {
        assert_eq!(rescope_volume("/srv/shared", "old", "new"), "/srv/shared");
        assert_eq!(rescope_volume("some_other_volume", "old", "new"), "some_other_volume");
    }

    #[test]
    fn resolves_relative_binds_to_host_paths() {
        assert_eq!(
            resolve_scoped_volume("projects/food/data", Some("/srv/litebin/projects")),
            "/srv/litebin/projects/food/data"
        );
        assert_eq!(resolve_scoped_volume("projects/food/data", None), "projects/food/data");
        assert_eq!(resolve_scoped_volume("litebin_food_pgdata", Some("/srv/litebin/projects")), "litebin_food_pgdata");
    }
}
