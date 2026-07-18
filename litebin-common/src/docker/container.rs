use std::collections::HashMap;

/// Returns true if the bind source is the Docker daemon socket.
/// Matches `/var/run/docker.sock`, `/run/docker.sock`, etc. — not other `.sock` files.
#[inline]
fn is_docker_sock(path: &str) -> bool {
    path.ends_with("/docker.sock")
}

pub(crate) fn sanitize_docker_socket_binds(
    binds: &[String],
    is_managed_proxy: bool,
) -> Vec<String> {
    if is_managed_proxy {
        return binds.to_vec();
    }
    binds
        .iter()
        .filter(|bind| {
            let source = bind.split(':').next().unwrap_or("");
            !is_docker_sock(source)
        })
        .cloned()
        .collect()
}

use bollard::models::{
    ContainerCreateBody, EndpointSettings, HostConfig, HostConfigLogConfig, NetworkingConfig,
    PortBinding, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use futures_util::StreamExt;

use crate::types::{
    RunServiceConfig, container_name, litebin_reserved_host_ports, parse_container_name,
    project_network_name,
};

use super::{DockerManager, RunningContainer};

impl DockerManager {
    /// Inspect a container and return the mapped host port.
    /// Returns `None` if no port mapping is found (container may have exited
    /// or port bindings haven't been applied yet).
    pub async fn inspect_mapped_port(&self, container_id: &str) -> anyhow::Result<Option<u16>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let port = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|ports| {
                ports.values().find_map(|bindings| {
                    bindings
                        .as_ref()?
                        .first()
                        .and_then(|b| b.host_port.as_ref().and_then(|p| p.parse::<u16>().ok()))
                })
            });
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
                if !cid.starts_with(&prefix) {
                    // avoid double-remove
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

        // When allow_raw_ports is set, bind compose-declared ports directly on the host
        // (e.g., UDP for game servers, TCP for databases). LiteBin-reserved ports are
        // always refused to avoid conflicts with Caddy/orchestrator/agent.
        let reserved_ports = litebin_reserved_host_ports();
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
                        if reserved_ports.iter().any(|p| p == host_port) {
                            tracing::warn!(
                                service = %config.service_name,
                                project_id = %config.project_id,
                                port = %host_port,
                                "skipping host bind for litebin-reserved port even with allow_raw_ports"
                            );
                            continue;
                        }
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

        // Workloads never receive the raw Docker socket. Only LiteBin's managed
        // observation proxy may mount it.
        let is_docker_proxy = config.service_name == crate::types::DOCKER_PROXY_SERVICE;
        let filtered_binds: Option<Vec<String>> = if is_docker_proxy {
            config.binds.clone()
        } else if let Some(ref binds) = config.binds {
            let stripped = sanitize_docker_socket_binds(binds, false);
            if stripped.len() != binds.len() {
                tracing::warn!(
                    service = %config.service_name,
                    project_id = %config.project_id,
                    "stripped raw Docker socket mount from workload"
                );
                Some(stripped)
            } else {
                config.binds.clone()
            }
        } else {
            config.binds.clone()
        };

        // Fix volume permissions for non-root containers.
        let image_user = self.inspect_image_user(&config.image).await;
        self.chown_bind_mounts(config, filtered_binds.as_ref(), image_user.as_deref());

        // Per-service resource limits (fall back to global defaults when not specified)
        let default_mem = self.memory_limit.load(std::sync::atomic::Ordering::Relaxed);
        let default_cpu = f64::from_bits(self.cpu_limit.load(std::sync::atomic::Ordering::Relaxed));
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
                "FOWNER".to_string(),
                "FSETID".to_string(),
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

            // The raw Docker socket is reserved exclusively for the managed proxy.
            if !is_docker_proxy {
                if let Some(ref binds) = host.binds {
                    let filtered = sanitize_docker_socket_binds(binds, false);
                    if filtered.len() != binds.len() {
                        tracing::warn!(
                            service = %config.service_name,
                            project_id = %config.project_id,
                            "stripped raw Docker socket mount from compose workload"
                        );
                        host.binds = Some(filtered);
                    }
                }
            }

            // Apply LiteBin binds (volume mounts)
            if let Some(ref binds) = filtered_binds {
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
                    map.insert(
                        net_name,
                        EndpointSettings {
                            aliases: Some(vec![config.service_name.clone()]),
                            ..Default::default()
                        },
                    );
                    map
                }),
            });

            // Set hostname to service name for DNS resolution within the network
            body.hostname = Some(config.service_name.clone());

            // Label all containers with project_id for docker-socket-proxy filtering,
            // and standard Docker Compose labels for tooling compatibility.
            // Skip litebin-docker-proxy itself so it can't be managed through its own proxy.
            if config.service_name != crate::types::DOCKER_PROXY_SERVICE {
                let mut labels = HashMap::new();
                labels.insert("litebin.project_id".to_string(), config.project_id.clone());
                labels.insert(
                    "com.docker.compose.service".to_string(),
                    config.service_name.clone(),
                );
                labels.insert(
                    "com.docker.compose.project".to_string(),
                    config.project_id.clone(),
                );
                if let Some(ref existing_labels) = body.labels {
                    labels.extend(existing_labels.clone());
                }
                body.labels = Some(labels);
            }

            body
        } else {
            // Single-service path: build from RunServiceConfig fields
            let mut translated_binds = filtered_binds;
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
                cmd: config.cmd.as_deref().and_then(|c| shlex::split(c)),
                labels: if config.service_name == crate::types::DOCKER_PROXY_SERVICE {
                    None
                } else {
                    Some({
                        let mut labels = HashMap::new();
                        labels.insert("litebin.project_id".to_string(), config.project_id.clone());
                        labels.insert(
                            "com.docker.compose.service".to_string(),
                            config.service_name.clone(),
                        );
                        labels.insert(
                            "com.docker.compose.project".to_string(),
                            config.project_id.clone(),
                        );
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

        // Get the mapped port for public services (non-fatal).
        // Containers that crash immediately (e.g. missing docker.sock) will
        // have no port mapping — return 0 and let status polling resolve it.
        let mapped_port = if config.is_public && config.port.is_some() {
            match self.inspect_mapped_port(&container_id).await {
                Ok(Some(port)) => port,
                Ok(None) => {
                    // Port key exists but binding is empty — container likely exited
                    let info = self.docker.inspect_container(&container_id, None).await?;
                    let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                    let exit_code = info.state.as_ref().and_then(|s| s.exit_code);
                    tracing::warn!(
                        service = %config.service_name,
                        container_id = %container_id,
                        running,
                        exit_code = ?exit_code,
                        "no mapped port found — container may have exited"
                    );
                    0
                }
                Err(e) => {
                    tracing::warn!(
                        service = %config.service_name,
                        container_id = %container_id,
                        error = %e,
                        "failed to inspect mapped port"
                    );
                    0
                }
            }
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

    /// Prepare bind mount directories for non-root containers.
    /// Creates directories and chowns them to the container's user:group
    /// so that non-root processes can write to them.
    #[cfg(unix)]
    fn chown_bind_mounts(
        &self,
        config: &RunServiceConfig,
        filtered_binds: Option<&Vec<String>>,
        image_user: Option<&str>,
    ) {
        // Resolve effective user: compose override > image default
        let effective_user = config
            .user
            .as_deref()
            .or_else(|| {
                config
                    .bollard_create_body
                    .as_ref()
                    .and_then(|b| b.user.as_deref())
            })
            .or(image_user);

        let Some(user_str) = effective_user else {
            return;
        };

        // Skip root user (handles "0", "0:0", "root")
        if user_str == "0" || user_str == "0:0" || user_str == "root" {
            return;
        }

        // Collect binds from both sources:
        // - config.binds: single-service path and litebin-scoped volumes
        // - bollard_host_config.binds: compose-mapped volumes (e.g., ./data:/app/data)
        let mut all_binds: Vec<&str> = Vec::new();
        if let Some(ref binds) = filtered_binds {
            all_binds.extend(binds.iter().map(|s| s.as_str()));
        }
        if let Some(ref hc) = config.bollard_host_config {
            if let Some(ref binds) = hc.binds {
                all_binds.extend(binds.iter().map(|s| s.as_str()));
            }
        }

        let host_dir = self.host_projects_dir.as_deref();
        let project_base = host_dir
            .map(|hd| std::path::Path::new(hd).canonicalize().ok())
            .flatten()
            .or_else(|| std::path::Path::new("projects").canonicalize().ok());

        for bind in &all_binds {
            let source = match bind.split(':').next() {
                Some(s) if s.starts_with("projects/") && !s.contains("..") => s,
                _ => continue,
            };

            let host_path = if let Some(hd) = host_dir {
                format!("{}/{}", hd, &source["projects/".len()..])
            } else {
                source.to_string()
            };

            if let Err(e) = std::fs::create_dir_all(&host_path) {
                tracing::warn!(path = %host_path, error = %e, "failed to create bind mount directory for non-root container");
                continue;
            }

            // Verify the resolved path stays within the project directory
            if let Some(ref base) = project_base {
                if let Ok(resolved) = std::path::Path::new(&host_path).canonicalize() {
                    if !resolved.starts_with(base) {
                        tracing::warn!(path = %host_path, resolved = %resolved.display(), base = %base.display(), "bind mount path escapes project directory, skipping chown");
                        continue;
                    }
                }
            }

            // Try chown first (works for numeric UIDs and usernames that exist on host).
            // Fall back to chmod 777 if chown fails (string username not on host).
            let chowned = std::process::Command::new("chown")
                .arg("-R")
                .arg(user_str)
                .arg(&host_path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            if chowned {
                tracing::info!(path = %host_path, user = %user_str, "chowned bind mount directory for non-root container");
            } else {
                // chown failed (likely string username not on host) — make writable instead
                match std::process::Command::new("chmod")
                    .arg("-R")
                    .arg("a+rw")
                    .arg(&host_path)
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        tracing::info!(path = %host_path, user = %user_str, "chmod bind mount directory (could not resolve user)");
                    }
                    Ok(output) => {
                        tracing::warn!(path = %host_path, error = %String::from_utf8_lossy(&output.stderr), "failed to chmod bind mount directory");
                    }
                    Err(e) => {
                        tracing::warn!(path = %host_path, error = %e, "failed to chmod bind mount directory");
                    }
                }
            }
        }
    }

    #[cfg(not(unix))]
    fn chown_bind_mounts(
        &self,
        _config: &RunServiceConfig,
        _filtered_binds: Option<&Vec<String>>,
        _image_user: Option<&str>,
    ) {
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
    ) -> impl StreamExt<Item = Result<bollard::container::LogOutput, bollard::errors::Error>>
    + Send
    + Unpin {
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
    pub async fn wait_for_healthy(
        &self,
        container_id: &str,
        expect_healthcheck: bool,
    ) -> anyhow::Result<()> {
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
                        let log_msg = h
                            .log
                            .as_ref()
                            .and_then(|logs| logs.last())
                            .and_then(|l| l.output.as_deref())
                            .unwrap_or("");
                        anyhow::bail!("container unhealthy: {}", log_msg);
                    }
                    _ => {} // EMPTY, NONE, STARTING — keep polling
                },
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("healthcheck timeout after 60s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Wait until a one-shot container exits successfully (exit code 0).
    /// Polls inspect every 500ms with a 10-minute timeout (migrations can be slow).
    pub async fn wait_for_completed_successfully(&self, container_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let state = info.state.as_ref();
            let running = state.and_then(|s| s.running).unwrap_or(false);
            if !running {
                let exit_code = state.and_then(|s| s.exit_code).unwrap_or(-1);
                if exit_code == 0 {
                    return Ok(());
                }
                anyhow::bail!("one-shot container exited with code {}", exit_code);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("one-shot container did not exit within 600s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Exit code of a container if it is not running; `None` if still running or unknown.
    pub async fn container_exit_code(&self, container_id: &str) -> anyhow::Result<Option<i64>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let state = info.state.as_ref();
        let running = state.and_then(|s| s.running).unwrap_or(false);
        if running {
            return Ok(None);
        }
        Ok(state.and_then(|s| s.exit_code))
    }

    /// Wait for a container to have a valid IP address on its network (not "invalid" or empty).
    /// Docker sometimes assigns "invalid" IP briefly after container creation.
    /// Polls every 200ms, timeout 10s.
    pub async fn wait_for_network_ready(&self, container_id: &str) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let info = self.docker.inspect_container(container_id, None).await?;
            let has_valid_ip = info
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .map(|nets| {
                    nets.values().any(|net| {
                        let ip = net.ip_address.as_deref().unwrap_or("");
                        !ip.is_empty() && ip != "invalid"
                    })
                })
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
        let options = ListContainersOptions {
            all: true,
            filters: Some(std::collections::HashMap::from([(
                "name".to_string(),
                vec![name.to_string()],
            )])),
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
            filters: Some(HashMap::from([(
                "name".to_string(),
                vec![prefix.to_string()],
            )])),
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
                if let Some((project_id, service_name, instance_id)) = parse_container_name(trimmed)
                {
                    // Extract ports from list response
                    let ports = c.ports.as_ref().and_then(|ports| {
                        ports
                            .iter()
                            .find_map(|p| match (p.private_port, p.public_port) {
                                (internal, Some(public)) => Some((internal, public)),
                                _ => None,
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
}
