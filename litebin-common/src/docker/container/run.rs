use std::collections::HashMap;

use bollard::models::{
    ContainerCreateBody, EndpointSettings, HostConfig, HostConfigLogConfig, NetworkingConfig, PortBinding,
    RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{CreateContainerOptions, StartContainerOptions};

use super::super::DockerManager;
use crate::types::{RunServiceConfig, container_name, litebin_reserved_host_ports, project_network_name};

use super::{merge_service_env, sanitize_docker_socket_binds, should_stabilize_startup};

pub(crate) fn managed_proxy_loopback_binding(config: &RunServiceConfig) -> Option<(String, PortBinding)> {
    (config.is_managed_docker_proxy && config.port == Some(2375)).then(|| {
        (
            "2375/tcp".to_string(),
            PortBinding { host_ip: Some("127.0.0.1".to_string()), host_port: Some("0".to_string()) },
        )
    })
}

/// Pinned (non-ephemeral) host ports in the binding map.
pub(crate) fn pinned_host_ports(port_bindings: &HashMap<String, Option<Vec<PortBinding>>>) -> Vec<u16> {
    let mut ports: Vec<u16> = port_bindings
        .values()
        .flatten()
        .flatten()
        .filter_map(|b| b.host_port.as_deref().and_then(|p| p.parse::<u16>().ok()))
        .filter(|&p| p != 0)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Merge LiteBin's `alias:host-gateway` entries into a container's extra_hosts.
/// Existing (user/compose-declared) entries win and keep their order; LiteBin
/// entries are appended only for hostnames not already declared (case-insensitive).
pub(crate) fn merge_extra_hosts(existing: Option<Vec<String>>, lb_entries: Vec<String>) -> Option<Vec<String>> {
    let mut merged = existing.unwrap_or_default();
    if lb_entries.is_empty() {
        return (!merged.is_empty()).then_some(merged);
    }
    let mut declared: std::collections::HashSet<String> =
        merged.iter().filter_map(|entry| entry.split(':').next()).map(|host| host.to_ascii_lowercase()).collect();
    for entry in lb_entries {
        let Some(host) = entry.split(':').next() else { continue };
        if declared.insert(host.to_ascii_lowercase()) {
            merged.push(entry);
        }
    }
    Some(merged)
}

/// Publish an explicit compose HOST:CONTAINER remap. A container port already
/// carrying the public proxy's loopback binding keeps it — the raw host port
/// is bound alongside so the proxy and the host publication both work.
fn apply_raw_port_override(
    port_bindings: &mut HashMap<String, Option<Vec<PortBinding>>>,
    exposed_ports: &mut Vec<String>,
    port_spec: &str,
    binding: PortBinding,
) {
    match port_bindings.get_mut(port_spec) {
        Some(existing) => match existing {
            Some(bindings) => bindings.push(binding),
            None => *existing = Some(vec![binding]),
        },
        None => {
            port_bindings.insert(port_spec.to_string(), Some(vec![binding]));
            exposed_ports.push(port_spec.to_string());
        }
    }
}

impl DockerManager {
    /// Run a service container using the unified `RunServiceConfig`.
    /// Returns (container_id, mapped_port). mapped_port is only meaningful for public services.
    pub async fn run_service_container(&self, config: &RunServiceConfig) -> anyhow::Result<(String, u16)> {
        let name = container_name(&config.project_id, &config.service_name, config.instance_id.as_deref());
        // The image may live locally under a mirror's rewritten reference —
        // pull resolution records it, and the container must use that name.
        let image = self.resolved_image_ref(&config.image);

        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed_ports: Vec<String> = Vec::new();

        if config.host_network {
            let compose_has_ports = config
                .bollard_create_body
                .as_ref()
                .and_then(|body| body.exposed_ports.as_ref())
                .is_some_and(|ports| !ports.is_empty());
            if config.is_public
                || config.port.is_some()
                || config.allow_raw_ports
                || compose_has_ports
                || config.networks.as_ref().is_some_and(|networks| !networks.is_empty())
            {
                anyhow::bail!(
                    "host-network service '{}' cannot declare ports, public routing, raw ports, or custom networks",
                    config.service_name
                );
            }
        }

        // Only bind a host port for public services that have a port defined
        if config.is_public
            && !config.host_network
            && let Some(port) = config.port
        {
            let port_str = format!("{}/tcp", port);
            port_bindings.insert(
                port_str.clone(),
                Some(vec![PortBinding { host_ip: Some("127.0.0.1".to_string()), host_port: Some("0".to_string()) }]),
            );
            exposed_ports.push(port_str);
        }

        // A managed observation proxy remains private/bridged. Host-network
        // requesters reach it only through an ephemeral loopback publication.
        if let Some((port_key, binding)) = managed_proxy_loopback_binding(config) {
            port_bindings.insert(port_key.clone(), Some(vec![binding]));
            exposed_ports.push(port_key);
        }

        // With allow_raw_ports, explicit HOST:CONTAINER remaps publish on the host
        // (e.g., UDP for game servers, TCP for databases). Container-only short
        // syntax never publishes. LiteBin-reserved ports are always refused to
        // avoid conflicts with Caddy/orchestrator/agent.
        let reserved_ports = litebin_reserved_host_ports();
        if config.allow_raw_ports {
            for (port_spec, host_port) in &config.raw_port_host_overrides {
                if reserved_ports.contains(&host_port.to_string()) {
                    tracing::warn!(
                        service = %config.service_name,
                        project_id = %config.project_id,
                        port = %host_port,
                        "skipping host bind for litebin-reserved port even with allow_raw_ports"
                    );
                    continue;
                }
                apply_raw_port_override(
                    &mut port_bindings,
                    &mut exposed_ports,
                    port_spec,
                    PortBinding { host_ip: Some("0.0.0.0".to_string()), host_port: Some(host_port.to_string()) },
                );
            }
        }

        // Workloads never receive the raw Docker socket. Only LiteBin's managed
        // observation proxy may mount it.
        let is_docker_proxy = config.is_managed_docker_proxy;
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
        let image_user = self.inspect_image_user(&image).await;
        self.chown_bind_mounts(config, filtered_binds.as_ref(), image_user.as_deref());

        // Per-service resource limits (fall back to global defaults when not specified).
        // A value <= 0 means explicitly unlimited (no Docker resource cap).
        let default_mem = self.memory_limit.load(std::sync::atomic::Ordering::Relaxed);
        let default_cpu = f64::from_bits(self.cpu_limit.load(std::sync::atomic::Ordering::Relaxed));
        let memory: Option<i64> = match config.memory_limit_mb {
            Some(mb) if mb <= 0 => None,
            Some(mb) => Some(mb * 1024 * 1024),
            None if default_mem <= 0 => None,
            None => Some(default_mem),
        };
        let nano_cpus: Option<i64> = match config.cpu_limit {
            Some(cpus) if cpus <= 0.0 => None,
            Some(cpus) => Some((cpus * 1_000_000_000.0) as i64),
            None if default_cpu <= 0.0 => None,
            None => Some((default_cpu * 1_000_000_000.0) as i64),
        };

        // Build LiteBin security overrides (shared by both paths)
        let lb_host_overrides = |host: &mut HostConfig| {
            if config.host_network {
                host.network_mode = Some("host".to_string());
                host.port_bindings = None;
            } else if !port_bindings.is_empty() {
                host.port_bindings = Some(port_bindings.clone());
            }
            host.memory = memory;
            host.nano_cpus = nano_cpus;
            // Only override restart policy if compose didn't specify one
            if host.restart_policy.is_none() {
                host.restart_policy =
                    Some(RestartPolicy { name: Some(RestartPolicyNameEnum::NO), ..Default::default() });
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
            // Same-host reachability (NAT hairpin workaround): map LiteBin's
            // aliases to the host gateway so containers can reach same-host
            // services through names that publicly resolve to this host.
            // User-declared entries for the same hostname win. Meaningless in
            // the host netns, so host-network services are skipped.
            if !config.host_network {
                let lb_entries: Vec<String> =
                    self.host_aliases().into_iter().map(|alias| format!("{alias}:host-gateway")).collect();
                host.extra_hosts = merge_extra_hosts(host.extra_hosts.take(), lb_entries);
                if let Some(servers) = self.effective_dns_servers() {
                    host.dns = Some(servers);
                    host.dns_options = Some(vec!["timeout:1".to_string(), "attempts:1".to_string()]);
                }
            }
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

        let create_body = if let (Some(mut body), Some(mut host)) =
            (config.bollard_create_body.clone(), config.bollard_host_config.clone())
        {
            // Compose path: use bollard config as base, apply LiteBin overrides
            lb_host_overrides(&mut host);

            // The raw Docker socket is reserved exclusively for the managed proxy.
            if !is_docker_proxy && let Some(ref binds) = host.binds {
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
                body.env = Some(merge_service_env(body.env.unwrap_or_default(), &config.env, config.docker_observe));
            }

            // Merge exposed ports: keep compose-declared ports, add LiteBin public port
            if config.host_network {
                body.exposed_ports = None;
            } else if !exposed_ports.is_empty() {
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

            // Connect to the managed networks selected by the run plan.
            if config.host_network {
                body.networking_config = None;
                body.hostname = None;
            } else {
                let networks = config.networks.clone().unwrap_or_else(|| {
                    vec![crate::types::NetworkConfig {
                        name: project_network_name(&config.project_id, config.instance_id.as_deref()),
                        aliases: Some(vec![config.service_name.clone()]),
                    }]
                });
                body.networking_config = Some(NetworkingConfig {
                    endpoints_config: Some({
                        let mut map = HashMap::new();
                        for network in networks {
                            map.insert(
                                network.name,
                                EndpointSettings { aliases: network.aliases, ..Default::default() },
                            );
                        }
                        map
                    }),
                });

                // Set hostname to service name for DNS resolution within the network
                body.hostname = Some(config.service_name.clone());
            }

            // Add LiteBin and standard Compose labels to workload containers.
            if !config.is_managed_docker_proxy {
                let mut labels = HashMap::new();
                labels.insert("litebin.project_id".to_string(), config.project_id.clone());
                labels.insert("com.docker.compose.service".to_string(), config.service_name.clone());
                let group = config.label_group.clone().unwrap_or_else(|| config.project_id.clone());
                labels.insert("com.docker.compose.project".to_string(), group);
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
                network_mode: if config.host_network {
                    Some("host".to_string())
                } else if config.networks.is_some() {
                    None
                } else {
                    Some(self.network.clone())
                },
                extra_hosts: config.extra_hosts.clone(),
                ..Default::default()
            };
            lb_host_overrides(&mut host_config);

            let mut env = config.env.clone();
            if let Some(port) = config.port {
                env.push(format!("PORT={}", port));
            }

            let networking_config =
                (!config.host_network).then_some(config.networks.as_ref()).flatten().map(|networks| NetworkingConfig {
                    endpoints_config: Some(
                        networks
                            .iter()
                            .map(|network| {
                                (
                                    network.name.clone(),
                                    EndpointSettings { aliases: network.aliases.clone(), ..Default::default() },
                                )
                            })
                            .collect(),
                    ),
                });

            ContainerCreateBody {
                image: Some(image.clone()),
                exposed_ports: if exposed_ports.is_empty() { None } else { Some(exposed_ports) },
                host_config: Some(host_config),
                env: if env.is_empty() { None } else { Some(env) },
                cmd: config.cmd.as_deref().and_then(shlex::split),
                hostname: (!config.host_network).then(|| config.service_name.clone()),
                networking_config,
                labels: if config.is_managed_docker_proxy {
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

        let options = CreateContainerOptions { name: Some(name.clone()), platform: String::new() };

        // Remove any existing container with the same name (handles orphaned containers
        // from failed previous deploys that aren't tracked in the DB)
        if let Ok(Some(existing_id)) = self.find_container_by_name(&name).await {
            let _ = self.stop_container(&existing_id).await;
            let _ = self.remove_container(&existing_id).await;
        }

        // Clear error before create when a pinned host port is already published.
        let pinned = pinned_host_ports(&port_bindings);
        if !pinned.is_empty() {
            let in_use = self.published_host_ports().await.unwrap_or_default();
            if let Some(port) = pinned.iter().find(|p| in_use.contains(p)) {
                anyhow::bail!(
                    "service '{}': host port {} is already in use on this node — change the HOST:CONTAINER mapping or expose the service via a route",
                    config.service_name,
                    port
                );
            }
        }

        let response = self.docker.create_container(Some(options), create_body).await?;
        let container_id = response.id;

        self.docker.start_container(&container_id, None::<StartContainerOptions>).await?;

        if should_stabilize_startup(config.host_network, config.is_oneshot, config.is_managed_docker_proxy) {
            self.wait_for_host_network_startup(&container_id, &config.service_name).await?;
        }

        // Get the mapped port for public services (non-fatal).
        // Containers that crash immediately (e.g. missing docker.sock) will
        // have no port mapping — return 0 and let status polling resolve it.
        let mapped_port_key =
            if config.is_managed_docker_proxy && config.port == Some(2375) { Some("2375/tcp") } else { None };
        let mapped_port = if (config.is_public && config.port.is_some()) || mapped_port_key.is_some() {
            let inspected = if let Some(port_key) = mapped_port_key {
                self.inspect_mapped_port_for(&container_id, port_key).await
            } else {
                self.inspect_mapped_port(&container_id).await
            };
            match inspected {
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
            .or_else(|| config.bollard_create_body.as_ref().and_then(|b| b.user.as_deref()))
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
        if let Some(binds) = filtered_binds {
            all_binds.extend(binds.iter().map(|s| s.as_str()));
        }
        if let Some(hc) = config.bollard_host_config.as_ref()
            && let Some(binds) = hc.binds.as_ref()
        {
            all_binds.extend(binds.iter().map(|s| s.as_str()));
        }

        let host_dir = self.host_projects_dir.as_deref();
        let project_base = host_dir
            .and_then(|hd| std::path::Path::new(hd).canonicalize().ok())
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
            if let Some(base) = project_base.as_ref()
                && let Ok(resolved) = std::path::Path::new(&host_path).canonicalize()
                && !resolved.starts_with(base)
            {
                tracing::warn!(path = %host_path, resolved = %resolved.display(), base = %base.display(), "bind mount path escapes project directory, skipping chown");
                continue;
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
                match std::process::Command::new("chmod").arg("-R").arg("a+rw").arg(&host_path).output() {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_extra_hosts_user_entries_win() {
        let merged = merge_extra_hosts(
            Some(vec!["app.example.com:10.0.0.5".into()]),
            vec!["app.example.com:host-gateway".into()],
        )
        .unwrap();
        assert_eq!(merged, vec!["app.example.com:10.0.0.5".to_string()]);
    }

    #[test]
    fn merge_extra_hosts_appends_and_dedups_case_insensitively() {
        let merged = merge_extra_hosts(
            Some(vec!["Host.Docker.Internal:1.2.3.4".into()]),
            vec![
                "host.docker.internal:host-gateway".into(),
                "dash.example.com:host-gateway".into(),
                "dash.example.com:host-gateway".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            merged,
            vec!["Host.Docker.Internal:1.2.3.4".to_string(), "dash.example.com:host-gateway".to_string(),]
        );
    }

    #[test]
    fn merge_extra_hosts_none_and_empty_is_none() {
        assert_eq!(merge_extra_hosts(None, vec![]), None);
        assert_eq!(merge_extra_hosts(Some(vec![]), vec![]), None);
    }

    #[test]
    fn merge_extra_hosts_appends_to_empty() {
        let merged = merge_extra_hosts(None, vec!["host.docker.internal:host-gateway".into()]).unwrap();
        assert_eq!(merged, vec!["host.docker.internal:host-gateway".to_string()]);
    }

    #[test]
    fn effective_dns_servers_requires_config_and_health() {
        let docker = DockerManager::new_for_tests();
        assert_eq!(docker.effective_dns_servers(), None);
        docker.set_dns_servers(Some(vec!["172.17.0.1".into(), "1.1.1.1".into()]));
        assert_eq!(docker.effective_dns_servers(), Some(vec!["172.17.0.1".to_string(), "1.1.1.1".to_string()]));
        docker.set_split_dns_healthy(false);
        assert_eq!(docker.effective_dns_servers(), None);
        docker.set_split_dns_healthy(true);
        docker.set_dns_servers(None);
        assert_eq!(docker.effective_dns_servers(), None);
    }

    #[tokio::test]
    #[ignore = "requires a local Docker daemon and pulls alpine"]
    async fn live_host_aliases_bake_into_etc_hosts() {
        let project_id = format!("hosts-inject-{}", std::process::id());
        let project = crate::types::Project {
            id: project_id.clone(),
            user_id: "test".into(),
            name: None,
            description: None,
            is_background: false,
            image: Some("alpine:3.20".into()),
            internal_port: None,
            mapped_port: None,
            container_id: None,
            node_id: None,
            status: crate::types::ProjectStatus::Stopped,
            cmd: Some("sleep 30".into()),
            memory_limit_mb: None,
            cpu_limit: None,
            custom_domain: None,
            volumes: None,
            auto_stop_enabled: false,
            auto_stop_timeout_mins: 0,
            auto_start_enabled: false,
            allow_raw_ports: false,
            allow_docker_access: false,
            last_active_at: None,
            service_count: None,
            service_summary: None,
            deploy_type: Some(crate::types::DeployType::Image),
            demo_parent_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let config = crate::types::RunServiceConfig::from_project(&project, Vec::new());
        let mut docker = DockerManager::new("bridge".into(), 128 * 1024 * 1024, 0.25).expect("docker manager");
        docker.detect_host_projects_dir().await;
        docker.set_host_aliases(vec!["host.docker.internal".into(), "alias-test.invalid".into()]);
        docker.pull_image_with_opts("alpine:3.20", false).await.expect("pull alpine");
        let (container_id, _) = docker.run_service_container(&config).await.expect("container started");

        let result: anyhow::Result<String> = async {
            let hosts = docker.exec_container(&container_id, &["cat", "/etc/hosts"]).await?;
            anyhow::Ok(hosts.output)
        }
        .await;

        let _ = docker.stop_container(&container_id).await;
        let _ = docker.remove_container(&container_id).await;
        let _ = docker.remove_project_network(&project_id, None).await;

        let hosts = result.expect("exec /etc/hosts");
        assert!(hosts.contains("host.docker.internal"), "host.docker.internal missing:\n{hosts}");
        assert!(hosts.contains("alias-test.invalid"), "platform alias missing:\n{hosts}");
        // host-gateway must be daemon-resolved to a real IP, not left literal.
        assert!(!hosts.contains("host-gateway"), "host-gateway not resolved by daemon:\n{hosts}");
    }

    #[test]
    fn pinned_host_ports_skips_ephemeral() {
        let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        bindings.insert(
            "3000/tcp".to_string(),
            Some(vec![PortBinding { host_ip: None, host_port: Some("3000".to_string()) }]),
        );
        bindings
            .insert("80/tcp".to_string(), Some(vec![PortBinding { host_ip: None, host_port: Some("0".to_string()) }]));
        assert_eq!(pinned_host_ports(&bindings), vec![3000]);
    }

    #[test]
    fn raw_override_publishes_alongside_public_binding() {
        let raw = |host: &str| PortBinding { host_ip: Some("0.0.0.0".to_string()), host_port: Some(host.to_string()) };
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        port_bindings.insert(
            "80/tcp".to_string(),
            Some(vec![PortBinding { host_ip: Some("127.0.0.1".to_string()), host_port: Some("0".to_string()) }]),
        );
        let mut exposed = Vec::new();

        apply_raw_port_override(&mut port_bindings, &mut exposed, "80/tcp", raw("8080"));
        let web = port_bindings["80/tcp"].as_ref().unwrap();
        assert_eq!(web.len(), 2);
        assert_eq!(web[0].host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(web[1].host_port.as_deref(), Some("8080"));
        assert!(exposed.is_empty());

        apply_raw_port_override(&mut port_bindings, &mut exposed, "6379/tcp", raw("6379"));
        assert_eq!(port_bindings["6379/tcp"].as_ref().unwrap()[0].host_port.as_deref(), Some("6379"));
        assert_eq!(exposed, vec!["6379/tcp"]);
        assert_eq!(pinned_host_ports(&port_bindings), vec![6379, 8080]);
    }
}
