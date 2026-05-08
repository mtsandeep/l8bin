use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::models::{
    ContainerCreateBody, EndpointSettings, HostConfig, HostConfigLogConfig, NetworkingConfig,
    PortBinding, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, ListContainersOptions, ListImagesOptions,
    LogsOptions, PruneImagesOptions, RemoveContainerOptions, RemoveImageOptions,
    RemoveVolumeOptions,
    StartContainerOptions, StatsOptions, StopContainerOptions,
};
use bollard::auth::DockerCredentials;
use bollard::Docker;
use futures_util::StreamExt;
use serde::{Serialize, Deserialize};

use crate::types::{
    container_name, project_network_name, RunServiceConfig,
};

/// Cached CPU stats sample for computing deltas between readings.
#[derive(Clone)]
struct CpuSample {
    total_usage: u64,
    system_cpu_usage: u64,
}

pub struct DockerManager {
    docker: Docker,
    network: String,
    memory_limit: Arc<AtomicI64>,
    cpu_limit: Arc<AtomicU64>, // f64 stored as bits
    /// Host-side path for /app/projects (detected via self-inspection).
    /// Used to translate container-internal bind mount paths to host paths.
    host_projects_dir: Option<String>,
    /// Cached CPU stat samples per container_id for delta computation.
    cpu_samples: Arc<Mutex<HashMap<String, CpuSample>>>,
}

impl Clone for DockerManager {
    fn clone(&self) -> Self {
        Self {
            docker: self.docker.clone(),
            network: self.network.clone(),
            memory_limit: Arc::clone(&self.memory_limit),
            cpu_limit: Arc::clone(&self.cpu_limit),
            host_projects_dir: self.host_projects_dir.clone(),
            cpu_samples: Arc::clone(&self.cpu_samples),
        }
    }
}

impl DockerManager {
    pub fn new(network: String, memory_limit: i64, cpu_limit: f64) -> anyhow::Result<Self> {
        let docker = Docker::connect_with_socket_defaults()?;
        Ok(Self {
            docker,
            network,
            memory_limit: Arc::new(AtomicI64::new(memory_limit)),
            cpu_limit: Arc::new(AtomicU64::new(cpu_limit.to_bits())),
            host_projects_dir: None,
            cpu_samples: Arc::new(Mutex::new(HashMap::new())),
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
            memory_limit: Arc::new(AtomicI64::new(0)),
            cpu_limit: Arc::new(AtomicU64::new(0.0_f64.to_bits())),
            host_projects_dir: None,
            cpu_samples: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Detect the host-side path for `/app/projects` by inspecting this container's
    /// own mount information via the Docker API. This allows bind mount sources
    /// (which Docker resolves on the host) to use the correct host path instead
    /// of the container-internal path.
    pub async fn detect_host_projects_dir(&mut self) {
        let hostname = match std::fs::read_to_string("/etc/hostname") {
            Ok(h) => h.trim().to_string(),
            Err(_) => return,
        };

        let inspect = match self.docker.inspect_container(&hostname, None).await {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!("could not inspect own container for host path detection: {e}");
                return;
            }
        };

        if let Some(mounts) = inspect.mounts {
            for mount in mounts {
                if mount.destination.as_deref() == Some("/app/projects") {
                    if let Some(source) = mount.source {
                        tracing::info!(host_projects_dir = %source, "detected host projects directory");
                        self.host_projects_dir = Some(source);
                    }
                    return;
                }
            }
        }
        tracing::debug!("no /app/projects mount found on this container");
    }

    /// Update the default memory and CPU limits used as fallbacks
    /// when per-service limits are not specified.
    pub fn update_defaults(&self, memory_limit: i64, cpu_limit: f64) {
        tracing::info!(memory_bytes = memory_limit, cpu = cpu_limit, "updating DockerManager defaults");
        self.memory_limit.store(memory_limit, Ordering::Relaxed);
        self.cpu_limit.store(cpu_limit.to_bits(), Ordering::Relaxed);
    }

    /// Translate container-internal `/app/projects/...` paths in bind specs
    /// to host-side paths, so Docker resolves them correctly on the host.
    fn translate_bind_paths(&self, binds: &mut [String]) {
        if let Some(ref host_dir) = self.host_projects_dir {
            for bind in binds.iter_mut() {
                if let Some(colon_pos) = bind.find(':') {
                    let source = &bind[..colon_pos];
                    if source.starts_with("/app/projects/") {
                        let new_source = format!("{}{}", host_dir, &source["/app/projects".len()..]);
                        *bind = format!("{}{}", new_source, &bind[colon_pos..]);
                    }
                }
            }
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
                let serveraddress = std::env::var("LITEBIN_REGISTRY_URL")
                    .unwrap_or_else(|_| "https://index.docker.io/v1/".to_string());
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
        if bytes < 0 { return "0B".to_string(); }
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

        let options = CreateImageOptions {
            from_image: Some(image_ref.clone()),
            ..Default::default()
        };

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
                                        Some(format!("{}: {} {}/{}", id, status, Self::format_bytes(current), Self::format_bytes(total)))
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

    /// Remove container by service name using the centralized naming convention.
    pub async fn remove_by_service_name(
        &self,
        project_id: &str,
        service_name: &str,
        instance_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let name = container_name(project_id, service_name, instance_id);
        self.remove_by_exact_name(&name).await
    }

    /// Remove a container by its exact Docker name (idempotent — no error if not found).
    async fn remove_by_exact_name(&self, name: &str) -> anyhow::Result<()> {
        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.to_string()]);
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

    /// Ensure a per-project Docker bridge network exists (idempotent).
    /// Uses the centralized naming convention from `project_network_name()`.
    pub async fn ensure_project_network(
        &self,
        project_id: &str,
        instance_id: Option<&str>,
    ) -> anyhow::Result<()> {
        use bollard::models::NetworkCreateRequest;

        let network_name = project_network_name(project_id, instance_id);

        let networks = self.docker.list_networks(None).await?;
        let exists = networks.iter().any(|n| {
            n.name
                .as_deref()
                .map(|name| name == network_name)
                .unwrap_or(false)
        });

        if !exists {
            self.docker
                .create_network(NetworkCreateRequest {
                    name: network_name.clone(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await?;
            tracing::info!(network = %network_name, "created per-project docker network");
        }

        Ok(())
    }

    /// Remove a per-project Docker network (idempotent).
    pub async fn remove_project_network(
        &self,
        project_id: &str,
        instance_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let network_name = project_network_name(project_id, instance_id);
        match self.docker.remove_network(&network_name).await {
            Ok(_) => {
                tracing::info!(network = %network_name, "removed per-project docker network");
                Ok(())
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("404") || err_str.contains("not found") {
                    tracing::debug!(network = %network_name, "network already gone");
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Remove a Docker named volume (ignores 404).
    pub async fn remove_volume(&self, name: &str) -> anyhow::Result<()> {
        match self.docker.remove_volume(name, None::<RemoveVolumeOptions>).await {
            Ok(_) => {
                tracing::info!(volume = %name, "removed docker volume");
                Ok(())
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("404") || err_str.contains("not found") {
                    tracing::debug!(volume = %name, "volume already gone");
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Remove a volume by its scoped name, handling Docker volumes, relative bind mounts,
    /// and absolute bind mounts appropriately.
    pub async fn remove_volume_by_name(&self, scoped_name: &str) -> anyhow::Result<()> {
        match crate::types::classify_volume(scoped_name) {
            crate::types::VolumeKind::DockerVolume => {
                self.remove_volume(scoped_name).await
            }
            crate::types::VolumeKind::RelativeBindMount => {
                let path = std::path::Path::new(scoped_name);
                if path.exists() {
                    std::fs::remove_dir_all(path)?;
                    tracing::info!(path = %scoped_name, "removed bind mount directory");
                }
                Ok(())
            }
            crate::types::VolumeKind::AbsoluteBindMount => {
                tracing::debug!(path = %scoped_name, "skipping absolute bind mount");
                Ok(())
            }
        }
    }

    /// Clean up all resources for a project: containers, volumes, network, and project directory.
    /// Used by both orchestrator (local delete) and agent (remote delete).
    pub async fn cleanup_project_resources(
        &self,
        project_id: &str,
        volumes: &[String],
    ) -> anyhow::Result<()> {
        // 1. Stop + remove all containers matching the project prefix
        let prefix = format!("litebin-{}.", project_id);
        if let Ok(container_ids) = self.list_containers_by_prefix(&prefix).await {
            for cid in &container_ids {
                let _ = self.stop_container(cid).await;
                let _ = self.remove_container(cid).await;
                tracing::info!(project = %project_id, container_id = %cid, "cleanup: removed container");
            }
        }

        // 2. Also try single-service container name
        let single_name = format!("litebin-{}", project_id);
        if let Ok(single_ids) = self.list_containers_by_prefix(&single_name).await {
            for cid in &single_ids {
                if !cid.starts_with(&prefix) { // avoid double-remove
                    let _ = self.stop_container(cid).await;
                    let _ = self.remove_container(cid).await;
                }
            }
        }

        // 3. Remove volumes
        for vol_name in volumes {
            if let Err(e) = self.remove_volume_by_name(vol_name).await {
                tracing::warn!(project = %project_id, volume = %vol_name, error = %e, "cleanup: failed to remove volume");
            }
        }

        // 4. Remove per-project network
        let _ = self.remove_project_network(project_id, None).await;

        // 5. Remove project directory if it exists
        let project_dir = std::path::Path::new("projects").join(project_id);
        if project_dir.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&project_dir) {
                tracing::warn!(project = %project_id, error = %e, "cleanup: failed to remove project directory");
            }
        }

        Ok(())
    }

    /// Connect a running container to a Docker network (idempotent).
    pub async fn connect_container_to_network(
        &self,
        container_name: &str,
        network_name: &str,
    ) -> anyhow::Result<()> {
        use bollard::models::{NetworkConnectRequest, EndpointSettings};

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
            Err(e) => {
                let err_str = e.to_string();
                // Already connected is fine
                if err_str.contains("already connected") || err_str.contains("already exists in network") {
                    tracing::debug!(
                        container = %container_name,
                        network = %network_name,
                        "container already on network"
                    );
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Read the compose.yaml for a project. Returns None if the file doesn't exist.
    pub fn read_compose(project_id: &str) -> Option<String> {
        let path = std::path::PathBuf::from("projects")
            .join(project_id)
            .join("compose.yaml");
        std::fs::read_to_string(&path).ok()
    }

    /// Run a service container using the unified `RunServiceConfig`.
    /// Returns (container_id, mapped_port). mapped_port is only meaningful for public services.
    pub async fn run_service_container(
        &self,
        config: &RunServiceConfig,
    ) -> anyhow::Result<(String, u16)> {
        let name = container_name(
            &config.project_id,
            &config.service_name,
            config.instance_id.as_deref(),
        );

        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed_ports: Vec<String> = Vec::new();

        // Only bind a host port for public services that have a port defined
        if config.is_public {
            if let Some(port) = config.port {
                let port_str = format!("{}/tcp", port);
                port_bindings.insert(
                    port_str.clone(),
                    Some(vec![PortBinding {
                        host_ip: Some("127.0.0.1".to_string()),
                        host_port: Some("0".to_string()),
                    }]),
                );
                exposed_ports.push(port_str);
            }
        }

        // When allow_raw_ports is set, bind all non-public compose-declared ports directly
        // on the host (e.g., UDP for game servers, TCP for databases). The public HTTP
        // port is already handled by Caddy and is skipped above.
        if config.allow_raw_ports {
            if let Some(ref bollard_body) = config.bollard_create_body {
                if let Some(ref compose_exposed) = bollard_body.exposed_ports {
                    for port_spec in compose_exposed {
                        // Skip ports already bound (e.g. public HTTP port)
                        if port_bindings.contains_key(port_spec) {
                            continue;
                        }
                        // Parse the port number from spec (e.g. "5432/udp" -> "5432")
                        let host_port = port_spec.split('/').next().unwrap_or("0");
                        port_bindings.insert(
                            port_spec.clone(),
                            Some(vec![PortBinding {
                                host_ip: Some("0.0.0.0".to_string()),
                                host_port: Some(host_port.to_string()),
                            }]),
                        );
                    }
                }
            }
        }

        // Block Docker socket mounts unless allow_docker_access is enabled
        if !config.allow_docker_access {
            if let Some(ref binds) = config.binds {
                for bind in binds {
                    let source = bind.split(':').next().unwrap_or("");
                    if source.ends_with(".sock") {
                        return Err(anyhow::anyhow!(
                            "Docker socket access requires enabling 'Allow Docker access' in project settings"
                        ));
                    }
                }
            }
        }

        // Per-service resource limits (fall back to global defaults when not specified)
        let default_mem = self.memory_limit.load(Ordering::Relaxed);
        let default_cpu = f64::from_bits(self.cpu_limit.load(Ordering::Relaxed));
        let memory = config
            .memory_limit_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(default_mem);
        let nano_cpus = match config.cpu_limit {
            Some(cpus) => (cpus * 1_000_000_000.0) as i64,
            None => (default_cpu * 1_000_000_000.0) as i64,
        };

        // Build LiteBin security overrides (shared by both paths)
        let lb_host_overrides = |host: &mut HostConfig| {
            if !port_bindings.is_empty() {
                host.port_bindings = Some(port_bindings.clone());
            }
            host.memory = Some(memory);
            host.nano_cpus = Some(nano_cpus);
            // Only override restart policy if compose didn't specify one
            if host.restart_policy.is_none() {
                host.restart_policy = Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::NO),
                    ..Default::default()
                });
            }
            host.cap_drop = Some(vec!["ALL".to_string()]);
            host.cap_add = Some(vec![
                "CHOWN".to_string(),
                "DAC_OVERRIDE".to_string(),
                "SETGID".to_string(),
                "SETUID".to_string(),
                "NET_BIND_SERVICE".to_string(),
                "KILL".to_string(),
            ]);
            host.security_opt = Some(vec!["no-new-privileges".to_string()]);
            host.pids_limit = Some(4096);
            host.log_config = Some(HostConfigLogConfig {
                config: Some({
                    let mut log_opts = HashMap::new();
                    log_opts.insert("max-size".to_string(), "10m".to_string());
                    log_opts.insert("max-file".to_string(), "3".to_string());
                    log_opts
                }),
                ..Default::default()
            });
        };

        let create_body = if let (Some(mut body), Some(mut host)) = (
            config.bollard_create_body.clone(),
            config.bollard_host_config.clone(),
        ) {
            // Compose path: use bollard config as base, apply LiteBin overrides
            lb_host_overrides(&mut host);

            // Apply LiteBin binds (volume mounts)
            if let Some(ref binds) = config.binds {
                let mut translated = binds.clone();
                self.translate_bind_paths(&mut translated);
                let mut existing = host.binds.unwrap_or_default();
                existing.extend(translated);
                host.binds = Some(existing);
            }

            // Apply LiteBin env overrides
            if !config.env.is_empty() {
                let mut existing = body.env.unwrap_or_default();
                existing.extend(config.env.iter().cloned());
                body.env = Some(existing);
            }

            // Merge exposed ports: keep compose-declared ports, add LiteBin public port
            if !exposed_ports.is_empty() {
                if let Some(ref compose_exposed) = body.exposed_ports {
                    let mut merged = compose_exposed.clone();
                    for ep in &exposed_ports {
                        if !merged.contains(ep) {
                            merged.push(ep.clone());
                        }
                    }
                    body.exposed_ports = Some(merged);
                } else {
                    body.exposed_ports = Some(exposed_ports);
                }
            }

            body.host_config = Some(host);

            // Connect to per-project network so services can resolve each other by name
            let net_name = project_network_name(&config.project_id, config.instance_id.as_deref());
            body.networking_config = Some(NetworkingConfig {
                endpoints_config: Some({
                    let mut map = HashMap::new();
                    map.insert(net_name, EndpointSettings {
                        aliases: Some(vec![config.service_name.clone()]),
                        ..Default::default()
                    });
                    map
                }),
            });

            // Set hostname to service name for DNS resolution within the network
            body.hostname = Some(config.service_name.clone());

            // Label all containers with project_id for docker-socket-proxy filtering,
            // and standard Docker Compose labels for tooling compatibility.
            // Skip litebin-docker-proxy itself so it can't be managed through its own proxy.
            if config.service_name != "litebin-docker-proxy" {
                let mut labels = HashMap::new();
                labels.insert("litebin.project_id".to_string(), config.project_id.clone());
                labels.insert("com.docker.compose.service".to_string(), config.service_name.clone());
                labels.insert("com.docker.compose.project".to_string(), config.project_id.clone());
                if let Some(ref existing_labels) = body.labels {
                    labels.extend(existing_labels.clone());
                }
                body.labels = Some(labels);
            }

            body
        } else {
            // Single-service path: build from RunServiceConfig fields
            let mut translated_binds = config.binds.clone();
            if let Some(ref mut binds) = translated_binds {
                self.translate_bind_paths(binds);
            }
            let mut host_config = HostConfig {
                binds: translated_binds,
                network_mode: Some(self.network.clone()),
                ..Default::default()
            };
            lb_host_overrides(&mut host_config);

            let mut env = config.env.clone();
            if let Some(port) = config.port {
                env.push(format!("PORT={}", port));
            }

            ContainerCreateBody {
                image: Some(config.image.clone()),
                exposed_ports: if exposed_ports.is_empty() {
                    None
                } else {
                    Some(exposed_ports)
                },
                host_config: Some(host_config),
                env: if env.is_empty() { None } else { Some(env) },
                cmd: config
                    .cmd
                    .as_deref()
                    .and_then(|c| shlex::split(c)),
                labels: if config.service_name == "litebin-docker-proxy" {
                    None
                } else {
                    Some({
                        let mut labels = HashMap::new();
                        labels.insert("litebin.project_id".to_string(), config.project_id.clone());
                        labels.insert("com.docker.compose.service".to_string(), config.service_name.clone());
                        labels.insert("com.docker.compose.project".to_string(), config.project_id.clone());
                        labels
                    })
                },
                ..Default::default()
            }
        };

        let options = CreateContainerOptions {
            name: Some(name.clone()),
            platform: String::new(),
        };

        // Remove any existing container with the same name (handles orphaned containers
        // from failed previous deploys that aren't tracked in the DB)
        if let Ok(Some(existing_id)) = self.find_container_by_name(&name).await {
            let _ = self.stop_container(&existing_id).await;
            let _ = self.remove_container(&existing_id).await;
        }

        let response = self
            .docker
            .create_container(Some(options), create_body)
            .await?;
        let container_id = response.id;

        self.docker
            .start_container(&container_id, None::<StartContainerOptions>)
            .await?;

        // Get the mapped port for public services
        let mapped_port = if config.is_public && config.port.is_some() {
            self.inspect_mapped_port(&container_id).await?
        } else {
            0
        };

        tracing::info!(
            container_id = %container_id,
            project = %config.project_id,
            service = %config.service_name,
            instance = ?config.instance_id,
            mapped_port = %mapped_port,
            "service container started"
        );

        Ok((container_id, mapped_port))
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        self.docker.ping().await?;
        Ok(())
    }

    /// Follow container logs (stdout + stderr) as a stream.
    /// Returns a stream of `bollard::container::LogOutput` items.
    pub fn follow_container_logs(
        &self,
        container_name: &str,
        since: Option<i64>,
    ) -> impl StreamExt<Item = Result<bollard::container::LogOutput, bollard::errors::Error>> + Send + Unpin {
        use bollard::query_parameters::LogsOptions;
        let options = LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            since: since.map(|s| s as i32).unwrap_or(0),
            until: 0,
            timestamps: false,
            tail: "0".to_string(),
        };
        self.docker.logs(container_name, Some(options))
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

    /// Wait for a container to become healthy (polls inspect every 500ms, timeout 60s).
    /// Returns Ok if healthy, or the last error if it becomes unhealthy or times out.
    /// When `expect_healthcheck` is true, keeps polling even if health is None (first
    /// check hasn't run yet). When false, returns immediately if no healthcheck exists.
    pub async fn wait_for_healthy(&self, container_id: &str, expect_healthcheck: bool) -> anyhow::Result<()> {
        use bollard::models::HealthStatusEnum;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let health = info.state.as_ref().and_then(|s| s.health.as_ref());
            match health {
                None => {
                    if !expect_healthcheck {
                        return Ok(()); // No healthcheck defined
                    }
                    // Healthcheck exists but first check hasn't run yet — keep polling
                }
                Some(h) => match &h.status {
                    Some(HealthStatusEnum::HEALTHY) => return Ok(()),
                    Some(HealthStatusEnum::UNHEALTHY) => {
                        let log_msg = h.log.as_ref()
                            .and_then(|logs| logs.last())
                            .and_then(|l| l.output.as_deref())
                            .unwrap_or("");
                        anyhow::bail!("container unhealthy: {}", log_msg);
                    }
                    _ => {} // EMPTY, NONE, STARTING — keep polling
                }
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("healthcheck timeout after 60s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Wait for a container to have a valid IP address on its network (not "invalid" or empty).
    /// Docker sometimes assigns "invalid" IP briefly after container creation.
    /// Polls every 200ms, timeout 10s.
    pub async fn wait_for_network_ready(&self, container_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let has_valid_ip = info.network_settings.as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .map(|nets| nets.values().any(|net| {
                    let ip = net.ip_address.as_deref().unwrap_or("");
                    !ip.is_empty() && ip != "invalid"
                }))
                .unwrap_or(false);

            if has_valid_ip {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("network readiness timeout after 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
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

    /// List container IDs whose name starts with the given prefix (includes stopped containers).
    pub async fn list_containers_by_prefix(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let options = ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([("name".to_string(), vec![prefix.to_string()])])),
            ..Default::default()
        };
        let containers = self.docker.list_containers(Some(options)).await?;
        Ok(containers.into_iter().filter_map(|c| c.id).collect())
    }

    /// List all running litebin containers. Returns parsed container info using the
    /// centralized naming convention (`litebin-{project_id}`, `litebin-{project_id}-{service}`, etc.).
    pub async fn list_running_litebin_containers(&self) -> anyhow::Result<Vec<RunningContainer>> {
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
                if let Some((project_id, service_name, instance_id)) =
                    crate::types::parse_container_name(trimmed)
                {
                    // Extract ports from list response
                    let ports = c.ports.as_ref().and_then(|ports| {
                        ports.iter().find_map(|p| {
                            match (p.private_port, p.public_port) {
                                (internal, Some(public)) => Some((internal, public)),
                                _ => None,
                            }
                        })
                    });
                    if let Some((internal_port, mapped_port)) = ports {
                        result.push(RunningContainer {
                            project_id,
                            service_name,
                            instance_id,
                            container_name: trimmed.to_string(),
                            internal_port,
                            mapped_port,
                        });
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

    /// Get container resource stats (CPU %, memory).
    /// CPU is computed as a delta between the current reading and the previous
    /// cached reading (per container_id). Returns 0% on the first call for a
    /// container since there is no previous sample to diff against.
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
            samples.insert(container_id.to_string(), CpuSample {
                total_usage: current_total,
                system_cpu_usage: current_system,
            });

            if let Some(prev) = prev {
                let cpu_delta = current_total as f64 - prev.total_usage as f64;
                let system_delta = current_system as f64 - prev.system_cpu_usage as f64;
                if system_delta > 0.0 {
                    (cpu_delta / system_delta) * num_cpus * 100.0
                } else {
                    0.0
                }
            } else {
                0.0 // First reading — no previous sample to diff against
            }
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
        let cpu_limit = info.host_config
            .and_then(|h| h.nano_cpus)
            .filter(|&n| n > 0)
            .map(|n| n as f64 / 1_000_000_000.0);
        Ok(DiskUsage { size_rw, size_root_fs, cpu_limit })
    }

    /// Load a Docker image from a tar stream (output of `docker save`).
    pub async fn load_image<S, E>(&self, tar_stream: S) -> anyhow::Result<()>
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
        let info = self.docker
            .inspect_image(image_ref)
            .await
            .map_err(|e| anyhow::anyhow!("image inspect failed: {e}"))?;
        info.id
            .ok_or_else(|| anyhow::anyhow!("image inspect returned no id"))
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

/// Info about a running litebin container, returned by `list_running_litebin_containers`.
#[derive(Debug, Clone, Serialize)]
pub struct RunningContainer {
    pub project_id: String,
    pub service_name: String,
    pub instance_id: Option<String>,
    pub container_name: String,
    pub internal_port: u16,
    pub mapped_port: u16,
}

#[derive(Debug, Serialize)]
pub struct ContainerStats {
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiskUsage {
    pub size_rw: u64,              // Writable layer size (bytes written by container)
    pub size_root_fs: u64,         // Total image + writable layer
    pub cpu_limit: Option<f64>,    // CPU limit from HostConfig.NanoCpus (e.g. 1.5 = 1.5 cores)
}

pub fn is_port_ready(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}
