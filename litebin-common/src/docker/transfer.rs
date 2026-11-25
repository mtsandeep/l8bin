//! Transfer payloads for project migration: Docker volumes, project
//! directories, and images — streamed as tars through a never-started
//! busybox container's archive API (volume at `/volume`, projects dir at
//! `/projects`).

use std::collections::HashMap;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions, RemoveContainerOptions,
    UploadToContainerOptions,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};

use super::DockerManager;

pub const TRANSFER_TOOL_IMAGE: &str = "busybox:1.36";

/// Name prefix of the helper containers transfers read and write through.
pub const TRANSFER_HELPER_PREFIX: &str = "l8b-transfer-";

pub const TRANSFER_KIND_VOLUME: &str = "volume";
pub const TRANSFER_KIND_PROJECT_DIR: &str = "project-dir";
pub const TRANSFER_KIND_IMAGE: &str = "image";

/// One migration transfer payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferKind {
    /// Docker named volume, mounted at `/volume`.
    Volume(String),
    /// Project directory under the host projects dir, mounted at `/projects`.
    ProjectDir(String),
    /// Image reference, exported via `docker save` semantics.
    Image(String),
}

impl TransferKind {
    /// Parse from wire values (`kind`, `name`).
    pub fn parse(kind: &str, name: &str) -> Option<Self> {
        match kind {
            TRANSFER_KIND_VOLUME => Some(Self::Volume(name.to_string())),
            TRANSFER_KIND_PROJECT_DIR => Some(Self::ProjectDir(name.to_string())),
            TRANSFER_KIND_IMAGE => Some(Self::Image(name.to_string())),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Volume(_) => TRANSFER_KIND_VOLUME,
            Self::ProjectDir(_) => TRANSFER_KIND_PROJECT_DIR,
            Self::Image(_) => TRANSFER_KIND_IMAGE,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Volume(name) | Self::ProjectDir(name) | Self::Image(name) => name,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Volume(name) => format!("volume {name}"),
            Self::ProjectDir(pid) => format!("project directory {pid}"),
            Self::Image(image) => format!("image {image}"),
        }
    }
}

impl DockerManager {
    /// Host-side path of the projects directory (bind-mount source on this node).
    pub fn projects_host_dir(&self) -> Option<&str> {
        self.host_projects_dir.as_deref()
    }

    /// A never-started busybox container carrying the binds the archive API needs.
    async fn create_transfer_container(&self, binds: Vec<String>) -> anyhow::Result<String> {
        self.pull_image_with_opts(TRANSFER_TOOL_IMAGE, false).await?;
        let name = format!("{TRANSFER_HELPER_PREFIX}{:x}", rand::random::<u64>());
        let host_config = HostConfig {
            binds: Some(binds),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            memory: Some(128 * 1024 * 1024),
            nano_cpus: Some(500_000_000),
            ..Default::default()
        };
        let config = ContainerCreateBody {
            image: Some(TRANSFER_TOOL_IMAGE.to_string()),
            host_config: Some(host_config),
            open_stdin: Some(false),
            tty: Some(false),
            ..Default::default()
        };
        let options = CreateContainerOptions { name: Some(name), platform: String::new() };
        let result = self.docker.create_container(Some(options), config).await?;
        Ok(result.id)
    }

    /// Stream a transfer payload out of this node's Docker as a tar.
    /// The stream removes the helper container when it ends.
    pub async fn transfer_export(
        &self,
        kind: &TransferKind,
        projects_host_dir: Option<&str>,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, bollard::errors::Error>> + Send>>> {
        match kind {
            TransferKind::Image(image) => Ok(Box::pin(self.docker.export_image(image))),
            TransferKind::Volume(volume) => {
                self.docker.inspect_volume(volume).await?;
                let id = self.create_transfer_container(vec![format!("{volume}:/volume")]).await?;
                let options = DownloadFromContainerOptions { path: "/volume".to_string() };
                let stream = self.docker.download_from_container(&id, Some(options));
                Ok(Box::pin(with_container_cleanup(self.docker.clone(), id, stream)))
            }
            TransferKind::ProjectDir(project_id) => {
                let host =
                    projects_host_dir.ok_or_else(|| anyhow::anyhow!("projects host directory unknown on this node"))?;
                let id = self.create_transfer_container(vec![format!("{host}:/projects")]).await?;
                let options = DownloadFromContainerOptions { path: format!("/projects/{project_id}") };
                let stream = self.docker.download_from_container(&id, Some(options));
                Ok(Box::pin(with_container_cleanup(self.docker.clone(), id, stream)))
            }
        }
    }

    /// Stream a transfer payload tar into this node's Docker.
    pub async fn transfer_import<S>(
        &self,
        kind: &TransferKind,
        projects_host_dir: Option<&str>,
        tar: S,
    ) -> anyhow::Result<()>
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        match kind {
            TransferKind::Image(_) => {
                let tar = tar.map(Ok::<_, std::io::Error>).boxed();
                self.load_image(tar).await.map(|_| ())
            }
            TransferKind::Volume(volume) => {
                // The bind creates the volume if it does not exist yet.
                let helper = self.create_transfer_container(vec![format!("{volume}:/volume")]).await?;
                self.import_with_helper(helper, "/", tar)
                    .await
                    .map_err(|e| anyhow::anyhow!("volume import failed: {e}"))
            }
            TransferKind::ProjectDir(project_id) => {
                let host =
                    projects_host_dir.ok_or_else(|| anyhow::anyhow!("projects host directory unknown on this node"))?;
                let helper = self.create_transfer_container(vec![format!("{host}:/projects")]).await?;
                let _ = project_id; // archive's top-level dir carries the project id.
                self.import_with_helper(helper, "/projects", tar)
                    .await
                    .map_err(|e| anyhow::anyhow!("project directory import failed: {e}"))
            }
        }
    }

    /// Extract `tar` into a just-created helper, then remove the helper.
    async fn import_with_helper<S>(&self, helper: String, path: &str, tar: S) -> Result<(), bollard::errors::Error>
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        let mut cleanup = HelperCleanup::arm(self.docker.clone(), helper.clone());
        let options = UploadToContainerOptions { path: path.to_string(), ..Default::default() };
        let result = self.docker.upload_to_container(&helper, Some(options), bollard::body_stream(tar)).await;
        if remove_helper(&self.docker, &helper).await {
            cleanup.disarm();
        }
        result
    }

    /// Remove leftover helpers that still mount `volume` — they block its removal with "in use".
    pub(crate) async fn unpin_volume(&self, volume: &str) {
        let filters = HashMap::from([
            ("name".to_string(), vec![TRANSFER_HELPER_PREFIX.to_string()]),
            ("volume".to_string(), vec![volume.to_string()]),
        ]);
        let options = ListContainersOptions { all: true, filters: Some(filters), ..Default::default() };
        let Ok(containers) = self.docker.list_containers(Some(options)).await else { return };
        for id in containers.into_iter().filter_map(|container| container.id) {
            if remove_helper(&self.docker, &id).await {
                tracing::info!(volume, container = %id, "removed leftover transfer helper holding the volume");
            }
        }
    }

    /// Remove every transfer helper on the daemon. For process start only —
    /// helpers are never started, so any that exist were left by a killed predecessor.
    pub async fn reap_leaked_transfer_helpers(&self) {
        let Ok(helpers) = self.list_containers_by_prefix(TRANSFER_HELPER_PREFIX).await else { return };
        for id in helpers {
            if remove_helper(&self.docker, &id).await {
                tracing::info!(container = %id, "removed transfer helper left by a previous run");
            }
        }
    }
}

/// Force-remove a helper. `true` once it is gone (removed, or already absent).
async fn remove_helper(docker: &Docker, id: &str) -> bool {
    let options = RemoveContainerOptions { force: true, ..Default::default() };
    match docker.remove_container(id, Some(options)).await {
        Ok(()) => true,
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => true,
        Err(e) => {
            tracing::debug!(container = %id, error = %e, "transfer container cleanup failed");
            false
        }
    }
}

/// Removes a transfer helper on drop. Without it, a future dropped mid-transfer
/// (timeout, abort, a failed import side) leaves the `Created` helper holding its volume.
struct HelperCleanup {
    docker: Docker,
    id: String,
    /// Cleared once the helper is known to be gone.
    armed: bool,
}

impl HelperCleanup {
    fn arm(docker: Docker, id: String) -> Self {
        Self { docker, id, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HelperCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let (docker, id) = (self.docker.clone(), self.id.clone());
                runtime.spawn(async move {
                    remove_helper(&docker, &id).await;
                });
            }
            Err(_) => {
                tracing::warn!(container = %self.id, "transfer helper left behind — the startup sweep removes it")
            }
        }
    }
}

/// End the stream, then remove the helper.
fn with_container_cleanup<S>(
    docker: Docker,
    container_id: String,
    stream: S,
) -> impl Stream<Item = Result<Bytes, bollard::errors::Error>> + Send + 'static
where
    S: Stream<Item = Result<Bytes, bollard::errors::Error>> + Send + 'static,
{
    let cleanup = HelperCleanup::arm(docker, container_id);
    let stream = stream.fuse();
    let stream = Box::pin(stream);
    futures_util::stream::unfold((stream, Some(cleanup)), |(mut stream, mut cleanup)| async move {
        match stream.next().await {
            Some(item) => Some((item, (stream, cleanup))),
            None => {
                // Awaited so the removal precedes whatever the caller does next.
                if let Some(cleanup) = cleanup.as_mut()
                    && remove_helper(&cleanup.docker, &cleanup.id).await
                {
                    cleanup.disarm();
                }
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_kind_round_trips() {
        for kind in [
            TransferKind::Volume("litebin_app_data".into()),
            TransferKind::ProjectDir("myapp".into()),
            TransferKind::Image("sha256:abc".into()),
        ] {
            let parsed = TransferKind::parse(kind.kind(), kind.name()).unwrap();
            assert_eq!(parsed, kind);
        }
        assert!(TransferKind::parse("bogus", "x").is_none());
    }
}
