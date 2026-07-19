use bollard::auth::DockerCredentials;
use bollard::models::{EndpointSettings, NetworkConnectRequest, NetworkCreateRequest};
use bollard::query_parameters::{
    CreateImageOptions, ImportImageOptions, ListImagesOptions, PruneImagesOptions, RemoveImageOptions,
    RemoveVolumeOptions, StatsOptions,
};
use futures_util::StreamExt;
use serde::Deserialize;

use crate::types::{VolumeKind, classify_volume, project_network_name};

use super::{ContainerStats, CpuSample, DiskUsage, DockerErrorKind, DockerManager};

impl DockerManager {
    pub async fn ensure_network(&self) -> anyhow::Result<()> {
        let networks = self.docker.list_networks(None).await?;
        let exists = networks.iter().any(|n| n.name.as_deref().map(|name| name == self.network).unwrap_or(false));

        if !exists {
            self.docker
                .create_network(NetworkCreateRequest {
                    name: self.network.clone(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await?;
            tracing::info!(network = %self.network, "created docker network");
        }

        Ok(())
    }

    /// Connect a container (e.g. the orchestrator) to all existing per-project networks.
    /// This ensures the orchestrator can proxy to multi-service containers after a restart.
    pub async fn connect_to_project_networks(&self, container_name: &str) {
        match self.docker.list_networks(None).await {
            Ok(networks) => {
                for net in networks {
                    if let Some(name) = net.name.as_deref() {
                        if name.starts_with("litebin-") && name != "litebin-network" && name != self.network {
                            if let Err(e) = self.connect_container_to_network(container_name, name).await {
                                tracing::warn!(network = name, error = %e, "failed to connect to project network");
                            }
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to list networks"),
        }
    }

    /// Pull a Docker image, optionally forcing a registry check.
    /// When `force` is false, checks if the image exists locally first and skips the pull.
    /// Read Docker registry credentials from LITEBIN_REGISTRY_URL + LITEBIN_REGISTRY_AUTH env vars.
    /// Falls back to ~/.docker/config.json for inline auth entries.
    fn read_docker_credentials() -> Option<DockerCredentials> {
        // 1. Check env vars (base64 user:password)
        if let Ok(auth_b64) = std::env::var("LITEBIN_REGISTRY_AUTH") {
            if !auth_b64.is_empty() {
                let serveraddress =
                    std::env::var("LITEBIN_REGISTRY_URL").unwrap_or_else(|_| "https://index.docker.io/v1/".to_string());
                return Some(DockerCredentials {
                    auth: Some(auth_b64),
                    serveraddress: Some(serveraddress),
                    ..Default::default()
                });
            }
        }

        // 2. Read ~/.docker/config.json for inline auth
        let home = std::env::var("HOME").ok()?;
        let path = std::path::Path::new(&home).join(".docker/config.json");
        let content = std::fs::read_to_string(&path).ok()?;

        #[derive(Deserialize)]
        struct DockerConfig {
            auths: Option<std::collections::HashMap<String, serde_json::Value>>,
        }

        let config: DockerConfig = serde_json::from_str(&content).ok()?;
        let auths = config.auths?;

        for key in &["https://index.docker.io/v1/", "https://registry-1.docker.io/v1/"] {
            if let Some(entry) = auths.get(*key) {
                let auth = entry.get("auth")?.as_str()?;
                if !auth.is_empty() {
                    return Some(DockerCredentials {
                        auth: Some(auth.to_string()),
                        serveraddress: Some(key.to_string()),
                        ..Default::default()
                    });
                }
            }
        }
        None
    }

    fn format_bytes(bytes: i64) -> String {
        if bytes < 0 {
            return "0B".to_string();
        }
        let b = bytes as u64;
        if b >= 1024 * 1024 * 1024 {
            format!("{:.1}GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
        } else if b >= 1024 * 1024 {
            format!("{:.1}MB", b as f64 / (1024.0 * 1024.0))
        } else if b >= 1024 {
            format!("{:.1}KB", b as f64 / 1024.0)
        } else {
            format!("{}B", b)
        }
    }

    /// Pull a Docker image with an optional progress callback that receives Docker's
    /// native pull output (status messages, layer progress, etc.).
    pub async fn pull_image_with_progress(
        &self,
        image: &str,
        force: bool,
        on_progress: Option<Box<dyn Fn(&str) + Send + Sync>>,
    ) -> anyhow::Result<()> {
        // Docker Engine API pulls ALL tags when no tag is specified — always default to :latest
        let image_ref = if image.contains(':') && !image.starts_with("sha256:") {
            image.to_string()
        } else {
            format!("{}:latest", image)
        };

        if !force {
            match self.docker.inspect_image(&image_ref).await {
                Ok(_) => {
                    tracing::info!(image = %image_ref, "image exists locally, skipping pull");
                    if let Some(ref cb) = on_progress {
                        cb(&format!("Image {} exists locally, skipping pull", image_ref));
                    }
                    return Ok(());
                }
                Err(_) => {
                    tracing::debug!(image = %image_ref, "image not found locally, will pull");
                }
            }
        }

        tracing::info!(image = %image_ref, "pulling image");

        let options = CreateImageOptions { from_image: Some(image_ref.clone()), ..Default::default() };

        let credentials = Self::read_docker_credentials();
        let mut stream = self.docker.create_image(Some(options), None, credentials);

        let mut last_pct: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = &info.status {
                        // Format message like Docker CLI
                        let msg = if let Some(id) = &info.id {
                            if let Some(pd) = &info.progress_detail {
                                let current = pd.current.unwrap_or(0);
                                let total = pd.total.unwrap_or(0);
                                if total > 0 {
                                    let pct = (current as f64 / total as f64 * 100.0) as u64;
                                    // Only emit progress lines at every 10% boundary to avoid flooding
                                    let prev = last_pct.get(id).copied().unwrap_or(0);
                                    let should_emit = pct / 10 > prev / 10;
                                    last_pct.insert(id.clone(), pct);
                                    if should_emit {
                                        Some(format!(
                                            "{}: {} {}/{}",
                                            id,
                                            status,
                                            Self::format_bytes(current),
                                            Self::format_bytes(total)
                                        ))
                                    } else {
                                        None
                                    }
                                } else {
                                    last_pct.remove(id);
                                    Some(format!("{}: {}", id, status))
                                }
                            } else {
                                last_pct.remove(id);
                                Some(format!("{}: {}", id, status))
                            }
                        } else {
                            Some(status.clone())
                        };

                        if let Some(m) = msg {
                            tracing::debug!(image = %image_ref, "pull: {}", m);
                            if let Some(ref cb) = on_progress {
                                cb(&m);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(image = %image_ref, error = %e, "pull failed");
                    return Err(e.into());
                }
            }
        }

        tracing::info!(image = %image_ref, "image pulled successfully");
        Ok(())
    }

    /// Pull a Docker image, optionally skipping if it exists locally.
    pub async fn pull_image_with_opts(&self, image: &str, force: bool) -> anyhow::Result<()> {
        self.pull_image_with_progress(image, force, None).await
    }

    /// Pull a Docker image (always contacts the registry).
    pub async fn pull_image(&self, image: &str) -> anyhow::Result<()> {
        self.pull_image_with_opts(image, true).await
    }

    /// Ensure a per-project Docker bridge network exists (idempotent).
    /// Uses the centralized naming convention from `project_network_name()`.
    pub async fn ensure_project_network(&self, project_id: &str, instance_id: Option<&str>) -> anyhow::Result<()> {
        let network_name = project_network_name(project_id, instance_id);
        self.ensure_named_network(&network_name).await
    }

    pub async fn ensure_named_network(&self, network_name: &str) -> anyhow::Result<()> {
        let networks = self.docker.list_networks(None).await?;
        let exists = networks.iter().any(|n| n.name.as_deref().map(|name| name == network_name).unwrap_or(false));

        if !exists {
            self.docker
                .create_network(NetworkCreateRequest {
                    name: network_name.to_string(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await?;
            tracing::info!(network = %network_name, "created per-project docker network");
        }

        Ok(())
    }

    /// Remove a per-project Docker network (idempotent).
    pub async fn remove_project_network(&self, project_id: &str, instance_id: Option<&str>) -> anyhow::Result<()> {
        let network_name = project_network_name(project_id, instance_id);
        self.remove_named_network(&network_name).await
    }

    pub async fn remove_named_network(&self, network_name: &str) -> anyhow::Result<()> {
        match self.docker.remove_network(network_name).await {
            Ok(_) => {
                tracing::info!(network = %network_name, "removed per-project docker network");
                Ok(())
            }
            Err(e) => match DockerErrorKind::from_bollard_error(&e) {
                DockerErrorKind::NotFound => {
                    tracing::debug!(network = %network_name, "network already gone");
                    Ok(())
                }
                _ => Err(e.into()),
            },
        }
    }

    /// Remove a Docker named volume (ignores 404).
    pub async fn remove_volume(&self, name: &str) -> anyhow::Result<()> {
        match self.docker.remove_volume(name, None::<RemoveVolumeOptions>).await {
            Ok(_) => {
                tracing::info!(volume = %name, "removed docker volume");
                Ok(())
            }
            Err(e) => match DockerErrorKind::from_bollard_error(&e) {
                DockerErrorKind::NotFound => {
                    tracing::debug!(volume = %name, "volume already gone");
                    Ok(())
                }
                _ => Err(e.into()),
            },
        }
    }

    /// Remove a volume by its scoped name, handling Docker volumes, relative bind mounts,
    /// and absolute bind mounts appropriately.
    pub async fn remove_volume_by_name(&self, scoped_name: &str) -> anyhow::Result<()> {
        match classify_volume(scoped_name) {
            VolumeKind::DockerVolume => self.remove_volume(scoped_name).await,
            VolumeKind::RelativeBindMount => {
                let path = std::path::Path::new(scoped_name);
                if path.exists() {
                    std::fs::remove_dir_all(path)?;
                    tracing::info!(path = %scoped_name, "removed bind mount directory");
                }
                Ok(())
            }
            VolumeKind::AbsoluteBindMount => {
                tracing::debug!(path = %scoped_name, "skipping absolute bind mount");
                Ok(())
            }
        }
    }

    /// Connect a running container to a Docker network (idempotent).
    pub async fn connect_container_to_network(&self, container_name: &str, network_name: &str) -> anyhow::Result<()> {
        let config = NetworkConnectRequest {
            container: container_name.to_string(),
            endpoint_config: Some(EndpointSettings::default()),
        };

        match self.docker.connect_network(network_name, config).await {
            Ok(_) => {
                tracing::info!(
                    container = %container_name,
                    network = %network_name,
                    "connected container to network"
                );
                Ok(())
            }
            Err(e) => match DockerErrorKind::from_bollard_error(&e) {
                DockerErrorKind::Forbidden => {
                    tracing::debug!(
                        container = %container_name,
                        network = %network_name,
                        "container already on network"
                    );
                    Ok(())
                }
                _ => Err(e.into()),
            },
        }
    }

    /// Returns total host memory in bytes as reported by Docker info.
    pub async fn system_memory(&self) -> anyhow::Result<i64> {
        let info = self.docker.info().await?;
        Ok(info.mem_total.unwrap_or(0))
    }

    /// Returns (cpu_cores, now_timestamp) from Docker info.
    pub async fn system_info(&self) -> anyhow::Result<(f64, i64)> {
        let info = self.docker.info().await?;
        let cpu = info.ncpu.unwrap_or(0) as f64;
        let now = chrono::Utc::now().timestamp();
        Ok((cpu, now))
    }

    /// Get container resource stats (CPU %, memory).
    /// CPU is computed as a delta between the current reading and the previous
    /// cached reading (per container_id). Returns 0% on the first call for a
    /// container since there is no previous sample to diff against.
    pub async fn container_stats(&self, container_id: &str) -> anyhow::Result<ContainerStats> {
        let stats = self
            .docker
            .stats(container_id, Some(StatsOptions { stream: false, one_shot: true }))
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("no stats returned"))??;

        // CPU usage % — compute delta from cached previous sample
        let cpu_stats = stats.cpu_stats.unwrap_or_default();
        let cpu_usage = cpu_stats.cpu_usage.unwrap_or_default();
        let current_total = cpu_usage.total_usage.unwrap_or(0);
        let current_system = cpu_stats.system_cpu_usage.unwrap_or(0);
        let num_cpus = cpu_stats.online_cpus.unwrap_or(1) as f64;

        // Read and update cached sample
        let cpu_percent = {
            let mut samples = self.cpu_samples.lock().unwrap();
            let prev = samples.get(container_id).map(|s| s.clone());
            samples.insert(
                container_id.to_string(),
                CpuSample { total_usage: current_total, system_cpu_usage: current_system },
            );

            if let Some(prev) = prev {
                let cpu_delta = current_total as f64 - prev.total_usage as f64;
                let system_delta = current_system as f64 - prev.system_cpu_usage as f64;
                if system_delta > 0.0 { (cpu_delta / system_delta) * num_cpus * 100.0 } else { 0.0 }
            } else {
                0.0 // First reading — no previous sample to diff against
            }
        };

        // Memory (subtract cache to match `docker stats`)
        let memory_stats = stats.memory_stats.unwrap_or_default();
        let cache = memory_stats.stats.as_ref().and_then(|s| s.get("inactive_file").copied()).unwrap_or(0);
        let memory_usage = memory_stats.usage.unwrap_or(0).saturating_sub(cache);
        let memory_limit = memory_stats.limit.unwrap_or(0);

        Ok(ContainerStats { cpu_percent: (cpu_percent * 100.0).round() / 100.0, memory_usage, memory_limit })
    }

    /// Get container disk usage (writable layer + rootfs)
    pub async fn disk_usage(&self, container_id: &str) -> anyhow::Result<DiskUsage> {
        let opts = bollard::query_parameters::InspectContainerOptions { size: true };
        let info = self.docker.inspect_container(container_id, Some(opts)).await?;
        let size_rw = info.size_rw.unwrap_or(0) as u64;
        let size_root_fs = info.size_root_fs.unwrap_or(0) as u64;
        let cpu_limit =
            info.host_config.and_then(|h| h.nano_cpus).filter(|&n| n > 0).map(|n| n as f64 / 1_000_000_000.0);
        Ok(DiskUsage { size_rw, size_root_fs, cpu_limit })
    }

    /// Load a Docker image from a tar stream (output of `docker save`).
    pub async fn load_image<S, E>(&self, tar_stream: S) -> anyhow::Result<()>
    where
        S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, E>> + Send + Unpin + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + std::fmt::Display + 'static,
    {
        let mut import_stream = self.docker.import_image_stream(ImportImageOptions::default(), tar_stream, None);

        while let Some(result) = import_stream.next().await {
            if let Err(e) = result {
                return Err(anyhow::anyhow!("docker image load failed: {e}"));
            }
        }

        tracing::info!("image loaded successfully");
        Ok(())
    }

    /// Inspect an image and return its actual image ID as known by Docker.
    /// Use a tag (e.g. "l8b/app:latest") to resolve to the correct ID,
    /// which may differ from the local config digest for OCI format images.
    pub async fn inspect_image_id(&self, image_ref: &str) -> anyhow::Result<String> {
        let info =
            self.docker.inspect_image(image_ref).await.map_err(|e| anyhow::anyhow!("image inspect failed: {e}"))?;
        info.id.ok_or_else(|| anyhow::anyhow!("image inspect returned no id"))
    }

    /// Inspect an image and return its configured user (from Dockerfile USER directive).
    /// Returns None if the image has no USER or on any error (image not found, etc.).
    pub async fn inspect_image_user(&self, image_ref: &str) -> Option<String> {
        let info = self.docker.inspect_image(image_ref).await.ok()?;
        info.config?.user
    }

    /// Compute image statistics: dangling count/size, in-use count/size, total.
    /// A "dangling" image is one with no repo tags (untagged).
    pub async fn image_stats(&self) -> crate::types::ImageStats {
        let images = match self.docker.list_images(Some(ListImagesOptions::default())).await {
            Ok(imgs) => imgs,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list images");
                return crate::types::ImageStats::default();
            }
        };

        let mut dangling_count = 0u64;
        let mut dangling_size = 0u64;
        let mut in_use_count = 0u64;
        let mut in_use_size = 0u64;

        for img in &images {
            let is_dangling = img.repo_tags.is_empty();
            let size = img.size as u64;

            if is_dangling {
                dangling_count += 1;
                dangling_size += size;
            } else {
                in_use_count += 1;
                in_use_size += size;
            }
        }

        crate::types::ImageStats {
            dangling_count,
            dangling_size,
            in_use_count,
            in_use_size,
            total_count: dangling_count + in_use_count,
            total_size: dangling_size + in_use_size,
        }
    }

    /// Remove an image by ID or tag if it is not used by any container.
    /// Returns true if the image was removed, false if still in use or not found.
    pub async fn remove_unused_image(&self, image_ref: &str) -> anyhow::Result<bool> {
        // List all images to find the matching one and check if it's in use
        let images = self.docker.list_images(Some(ListImagesOptions::default())).await?;

        let mut target_id: Option<String> = None;

        for img in &images {
            // Check by ID prefix match or repo tag match
            let matches = img.id.starts_with(image_ref)
                || img.repo_tags.iter().any(|t| t == image_ref)
                || img.repo_digests.iter().any(|d| d == image_ref);

            if matches {
                if img.containers > 0 {
                    tracing::debug!(image = %image_ref, "image still in use by {} containers, skipping removal", img.containers);
                    return Ok(false);
                }
                target_id = Some(img.id.clone());
            }
        }

        let Some(id) = target_id else {
            tracing::debug!(image = %image_ref, "image not found, nothing to remove");
            return Ok(false);
        };

        let opts = RemoveImageOptions::default();
        match self.docker.remove_image(&id, Some(opts), None).await {
            Ok(_) => {
                tracing::info!(image = %image_ref, "unused image removed");
                Ok(true)
            }
            Err(e) => match DockerErrorKind::from_bollard_error(&e) {
                DockerErrorKind::NotFound => {
                    tracing::debug!(image = %image_ref, "image already gone");
                    Ok(false)
                }
                _ => Err(e.into()),
            },
        }
    }

    /// Prune all dangling (untagged and unused) images. Returns bytes reclaimed.
    pub async fn prune_dangling_images(&self) -> anyhow::Result<u64> {
        let opts = PruneImagesOptions::default();
        let result = self.docker.prune_images(Some(opts)).await?;
        let reclaimed = result.space_reclaimed.unwrap_or(0) as u64;
        tracing::info!(reclaimed_bytes = reclaimed, "pruned dangling images");
        Ok(reclaimed)
    }
}
