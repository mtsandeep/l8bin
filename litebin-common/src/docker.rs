use std::collections::HashMap;
use std::time::Duration;

use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigLogConfig, PortBinding, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, ListContainersOptions, ListImagesOptions,
    LogsOptions, PruneImagesOptions, RemoveContainerOptions, RemoveImageOptions,
    StartContainerOptions, StatsOptions, StopContainerOptions,
};
use bollard::Docker;
use futures_util::StreamExt;
use serde::{Serialize, Deserialize};

use crate::types::Project;

#[derive(Clone)]
pub struct DockerManager {
    docker: Docker,
    network: String,
    memory_limit: i64,
    cpu_limit: f64,
}

impl DockerManager {
    pub fn new(network: String, memory_limit: i64, cpu_limit: f64) -> anyhow::Result<Self> {
        let docker = Docker::connect_with_socket_defaults()?;
        Ok(Self {
            docker,
            network,
            memory_limit,
            cpu_limit,
        })
    }

    /// Create a DockerManager without connecting to the Docker socket — for use in tests only.
    pub fn new_for_tests() -> Self {
        let docker = Docker::connect_with_http(
            "http://127.0.0.1:1",
            4,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("http docker client");
        Self {
            docker,
            network: "test".to_string(),
            memory_limit: 0,
            cpu_limit: 0.0,
        }
    }

    pub async fn ensure_network(&self) -> anyhow::Result<()> {
        use bollard::models::NetworkCreateRequest;

        let networks = self.docker.list_networks(None).await?;
        let exists = networks.iter().any(|n| {
            n.name
                .as_deref()
                .map(|name| name == self.network)
                .unwrap_or(false)
        });

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

    pub async fn pull_image(&self, image: &str) -> anyhow::Result<()> {
        tracing::info!(image = %image, "pulling image");

        let options = CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = &info.status {
                        tracing::debug!(status = %status, "pull progress");
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }

        tracing::info!(image = %image, "image pulled successfully");
        Ok(())
    }

    /// Run a container for a project, returning (container_id, mapped_port).
    /// If `mapped_port` is None, Docker auto-assigns the host port and this function
    /// inspects the container to retrieve it. If `mapped_port` is Some, that port is
    /// used directly (no inspect needed).
    pub async fn run_container(
        &self,
        project: &Project,
        extra_env: Vec<String>,
        mapped_port: Option<u16>,
    ) -> anyhow::Result<(String, u16)> {
        let container_name = format!("litebin-{}", project.id);

        let image = project.image.as_ref()
            .ok_or_else(|| anyhow::anyhow!("project '{}' has no image", project.id))?;
        let internal_port = project.internal_port
            .ok_or_else(|| anyhow::anyhow!("project '{}' has no port configured", project.id))?;

        let port_str = format!("{}/tcp", internal_port);
        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            port_str.clone(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(mapped_port.map_or("0".to_string(), |p| p.to_string())),
            }]),
        );

        // Per-project overrides fall back to DockerManager defaults
        let memory = project.memory_limit_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(self.memory_limit);
        let nano_cpus = project.cpu_limit
            .unwrap_or(self.cpu_limit);
        let nano_cpus = (nano_cpus * 1_000_000_000.0) as i64;

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            memory: Some(memory),
            nano_cpus: Some(nano_cpus),
            network_mode: Some(self.network.clone()),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::NO),
                ..Default::default()
            }),
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(vec![
                "CHOWN".to_string(),
                "DAC_OVERRIDE".to_string(),
                "SETGID".to_string(),
                "SETUID".to_string(),
                "NET_BIND_SERVICE".to_string(),
                "KILL".to_string(),
            ]),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            pids_limit: Some(4096),
            log_config: Some(HostConfigLogConfig {
                config: Some({
                    let mut log_opts = HashMap::new();
                    log_opts.insert("max-size".to_string(), "10m".to_string());
                    log_opts.insert("max-file".to_string(), "3".to_string());
                    log_opts
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: Some(image.clone()),
            exposed_ports: Some(vec![port_str]),
            host_config: Some(host_config),
            env: Some({
                let mut env = extra_env;
                env.push(format!("PORT={}", internal_port));
                env
            }),
            cmd: project.cmd.as_deref().and_then(|c| shlex::split(c)),
            ..Default::default()
        };

        let options = CreateContainerOptions {
            name: Some(container_name),
            platform: String::new(),
        };

        let response = self.docker.create_container(Some(options), config).await?;
        let container_id = response.id;

        self.docker
            .start_container(&container_id, None::<StartContainerOptions>)
            .await?;

        // Get the mapped port
        let port = match mapped_port {
            Some(p) => p,
            None => self.inspect_mapped_port(&container_id).await?,
        };

        tracing::info!(
            container_id = %container_id,
            project = %project.id,
            mapped_port = %port,
            "container started"
        );

        Ok((container_id, port))
    }

    /// Inspect a container and return the mapped host port.
    pub async fn inspect_mapped_port(&self, container_id: &str) -> anyhow::Result<u16> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let port = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|ports| {
                ports.values().find_map(|bindings| {
                    bindings.as_ref()?.first().and_then(|b| {
                        b.host_port.as_ref().and_then(|p| p.parse::<u16>().ok())
                    })
                })
            })
            .ok_or_else(|| anyhow::anyhow!("no mapped port found for container {}", container_id))?;
        Ok(port)
    }

    /// Start an existing stopped container (preserves port mappings)
    pub async fn start_existing_container(&self, container_id: &str) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "starting existing container");
        self.docker
            .start_container(container_id, None::<StartContainerOptions>)
            .await?;
        Ok(())
    }

    pub async fn stop_container(&self, container_id: &str) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "stopping container");
        self.docker
            .stop_container(
                container_id,
                Some(StopContainerOptions {
                    t: Some(2),
                    signal: None,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_container(&self, container_id: &str) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "removing container");
        self.docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;
        Ok(())
    }

    /// Remove container by project name (litebin-<project_id>)
    pub async fn remove_by_name(&self, project_id: &str) -> anyhow::Result<()> {
        let name = format!("litebin-{}", project_id);

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name]);
        let options = ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        };

        let containers = self.docker.list_containers(Some(options)).await?;
        for container in containers {
            if let Some(id) = container.id {
                self.remove_container(&id).await?;
            }
        }

        Ok(())
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        self.docker.ping().await?;
        Ok(())
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

    /// Check if a container is actually running in Docker
    pub async fn is_container_running(&self, container_id: &str) -> anyhow::Result<bool> {
        let info = self.docker.inspect_container(container_id, None).await?;
        Ok(info.state.and_then(|s| s.running).unwrap_or(false))
    }

    /// Inspect a container and return the raw bollard response
    pub async fn inspect_container(
        &self,
        container_id: &str,
    ) -> anyhow::Result<bollard::models::ContainerInspectResponse> {
        let info = self.docker.inspect_container(container_id, None).await?;
        Ok(info)
    }

    /// Find a container by its name (e.g. "litebin-myapp") and return its ID.
    /// Returns None if no container with that name exists (in any state).
    pub async fn find_container_by_name(&self, name: &str) -> anyhow::Result<Option<String>> {
        use bollard::query_parameters::ListContainersOptions;
        let options = ListContainersOptions {
            all: true,
            filters: Some(std::collections::HashMap::from([
                ("name".to_string(), vec![name.to_string()]),
            ])),
            ..Default::default()
        };
        let containers = self.docker.list_containers(Some(options)).await?;
        // Docker name filter is a substring match, so verify exact match
        for c in containers {
            if let Some(names) = &c.names {
                for n in names {
                    // Docker prefixes names with "/"
                    if n.trim_start_matches('/') == name {
                        return Ok(c.id.clone());
                    }
                }
            }
        }
        Ok(None)
    }

    /// Count running containers
    pub async fn running_container_count(&self) -> anyhow::Result<u32> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: false,
                ..Default::default()
            }))
            .await?;
        Ok(containers.len() as u32)
    }

    /// List all running litebin containers. Returns (project_id, mapped_port) for each.
    /// Containers are identified by name prefix "litebin-".
    pub async fn list_running_litebin_containers(&self) -> anyhow::Result<Vec<(String, u16)>> {
        let options = ListContainersOptions {
            all: false,
            ..Default::default()
        };
        let containers = self.docker.list_containers(Some(options)).await?;

        let mut result = Vec::new();
        for c in containers {
            let names = match &c.names {
                Some(n) => n,
                None => continue,
            };
            for name in names {
                let trimmed = name.trim_start_matches('/');
                if let Some(project_id) = trimmed.strip_prefix("litebin-") {
                    // Extract mapped port from list response (no per-container inspect needed)
                    let port = c.ports.as_ref().and_then(|ports| {
                        ports.iter().find_map(|p| p.public_port)
                    });
                    if let Some(port) = port {
                        result.push((project_id.to_string(), port));
                    }
                    break;
                }
            }
        }
        Ok(result)
    }

    /// Get container logs (last N lines)
    pub async fn container_logs(
        &self,
        container_id: &str,
        tail: usize,
    ) -> anyhow::Result<Vec<String>> {
        let opts = LogsOptions {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            ..Default::default()
        };

        let mut stream = self.docker.logs(container_id, Some(opts));
        let mut lines = Vec::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => lines.push(output.to_string()),
                Err(e) => return Err(e.into()),
            }
        }

        Ok(lines)
    }

    /// Get container resource stats (CPU %, memory)
    pub async fn container_stats(&self, container_id: &str) -> anyhow::Result<ContainerStats> {
        let stats = self
            .docker
            .stats(
                container_id,
                Some(StatsOptions {
                    stream: false,
                    one_shot: true,
                }),
            )
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("no stats returned"))??;

        // CPU usage %
        let cpu_stats = stats.cpu_stats.unwrap_or_default();
        let precpu_stats = stats.precpu_stats.unwrap_or_default();
        let cpu_usage = cpu_stats.cpu_usage.unwrap_or_default();
        let precpu_usage = precpu_stats.cpu_usage.unwrap_or_default();
        let cpu_delta = cpu_usage.total_usage.unwrap_or(0) as f64
            - precpu_usage.total_usage.unwrap_or(0) as f64;
        let system_delta = cpu_stats.system_cpu_usage.unwrap_or(0) as f64
            - precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
        let num_cpus = cpu_stats.online_cpus.unwrap_or(1) as f64;
        let cpu_percent = if system_delta > 0.0 {
            (cpu_delta / system_delta) * num_cpus * 100.0
        } else {
            0.0
        };

        // Memory (subtract cache to match `docker stats`)
        let memory_stats = stats.memory_stats.unwrap_or_default();
        let cache = memory_stats.stats
            .as_ref()
            .and_then(|s| s.get("inactive_file").copied())
            .unwrap_or(0);
        let memory_usage = memory_stats.usage.unwrap_or(0)
            .saturating_sub(cache);
        let memory_limit = memory_stats.limit.unwrap_or(0);

        Ok(ContainerStats {
            cpu_percent: (cpu_percent * 100.0).round() / 100.0,
            memory_usage,
            memory_limit,
        })
    }

    /// Get container disk usage (writable layer + rootfs)
    pub async fn disk_usage(&self, container_id: &str) -> anyhow::Result<DiskUsage> {
        let opts = bollard::query_parameters::InspectContainerOptions { size: true };
        let info = self.docker.inspect_container(container_id, Some(opts)).await?;
        let size_rw = info.size_rw.unwrap_or(0) as u64;
        let size_root_fs = info.size_root_fs.unwrap_or(0) as u64;
        Ok(DiskUsage { size_rw, size_root_fs })
    }

    /// Load a Docker image from a tar stream (output of `docker save`).
    /// Returns the image ID on success.
    pub async fn load_image<S, E>(&self, tar_stream: S) -> anyhow::Result<String>
    where
        S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, E>> + Send + Unpin + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + std::fmt::Display + 'static,
    {
        use bollard::query_parameters::ImportImageOptions;
        use futures_util::StreamExt;

        let mut import_stream = self.docker.import_image_stream(
            ImportImageOptions::default(),
            tar_stream,
            None,
        );

        let mut image_id = String::new();
        while let Some(result) = import_stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(id) = &info.id {
                        image_id = id.clone();
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("docker image import failed: {e}"));
                }
            }
        }

        if image_id.is_empty() {
            anyhow::bail!("image load completed but no image ID was returned");
        }

        tracing::info!(image_id = %image_id, "image loaded successfully");
        Ok(image_id)
    }

    /// Get the image ID for a given tag by inspecting it.
    pub async fn image_id_by_tag(&self, tag: &str) -> anyhow::Result<String> {
        let info = self.docker.inspect_image(tag).await?;
        info.id
            .ok_or_else(|| anyhow::anyhow!("image {} has no id", tag))
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
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("404") || err_str.contains("No such image") {
                    tracing::debug!(image = %image_ref, "image already gone");
                    Ok(false)
                } else {
                    Err(e.into())
                }
            }
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

#[derive(Debug, Serialize)]
pub struct ContainerStats {
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiskUsage {
    pub size_rw: u64,      // Writable layer size (bytes written by container)
    pub size_root_fs: u64, // Total image + writable layer
}

pub fn is_port_ready(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}
