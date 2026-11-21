use std::collections::HashMap;

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, HostConfigLogConfig, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{CreateContainerOptions, RestartContainerOptions, StartContainerOptions};
use futures_util::StreamExt;

use super::super::DockerManager;

/// Result of an `exec_container` call.
pub struct ExecOutput {
    pub exit_code: i64,
    /// Combined stdout+stderr.
    pub output: String,
}

/// One published port on a sidecar (e.g. DNS on the bridge gateway).
#[derive(Clone)]
pub struct SidecarPortPublish {
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
    /// "tcp" or "udp".
    pub protocol: &'static str,
}

/// Desired state for an orchestrator-managed sidecar container.
pub struct SidecarSpec {
    pub name: String,
    pub image: String,
    /// Exact image ID when the sidecar must track another container's image
    /// (self-image sidecars like dns); `None` compares by name only.
    pub image_id: Option<String>,
    /// Bind specs, e.g. `["myvolume:/data"]`.
    pub binds: Vec<String>,
    pub cmd: Vec<String>,
    /// Extra env (`KEY=value`) baked into the container at create time.
    pub env: Vec<String>,
    pub network: String,
    /// Value for the `litebin.sidecar` label (e.g. `backup`).
    pub label: String,
    /// Compose service label — groups the sidecar under `litebin-sidecars`.
    pub service: String,
    /// Published ports (empty = none).
    pub ports: Vec<SidecarPortPublish>,
    /// Override the default 256 MB ceiling (bytes).
    pub memory_limit: Option<i64>,
    /// Memory reservation (bytes) — set with the limit for tighter caps.
    pub memory_reservation: Option<i64>,
}

/// Compose project label for LiteBin-managed sidecars. Deliberately NOT the
/// platform's own project: `docker compose down` / `--remove-orphans` in the
/// install dir must never remove sidecars (the orchestrator's reconcile owns
/// their lifecycle). UIs still group them under this project.
pub const SIDECAR_COMPOSE_PROJECT: &str = "litebin-sidecars";

impl DockerManager {
    /// Volume name (or host path, for bind mounts) backing `destination` on this container.
    pub async fn detect_mount_bind_source(&self, destination: &str) -> Option<String> {
        let hostname = std::fs::read_to_string("/etc/hostname").ok()?.trim().to_string();
        let inspect = self.docker.inspect_container(&hostname, None).await.ok()?;
        for mount in inspect.mounts? {
            if mount.destination.as_deref() == Some(destination) {
                tracing::debug!(destination, "detected data mount for sidecar sharing");
                return mount.name.or(mount.source);
            }
        }
        None
    }

    /// This container's own image ID (SHA) — for sidecars that must track the
    /// platform binary's image (dns sidecar runs the orchestrator image).
    pub async fn self_image_id(&self) -> Option<String> {
        let hostname = std::fs::read_to_string("/etc/hostname").ok()?.trim().to_string();
        let inspect = self.docker.inspect_container(&hostname, None).await.ok()?;
        inspect.image
    }

    /// This container's own image NAME (as the daemon resolves it) — the
    /// create-time reference for self-image sidecars. `None` when not running
    /// inside a container (dev on the host).
    pub async fn self_image_name(&self) -> Option<String> {
        let hostname = std::fs::read_to_string("/etc/hostname").ok()?.trim().to_string();
        let inspect = self.docker.inspect_container(&hostname, None).await.ok()?;
        inspect.config?.image.filter(|image| !image.is_empty())
    }

    /// Gateway IP of the default bridge (`docker0`) — a host-local IP
    /// reachable from every container network; the bind target for the
    /// split-horizon DNS publish.
    pub async fn default_bridge_gateway(&self) -> Option<String> {
        let network = self.docker.inspect_network("bridge", None).await.ok()?;
        network.ipam?.config?.first().and_then(|c| c.gateway.clone()).filter(|g| !g.is_empty())
    }

    /// Restarts in place, keeping container identity — project services are recreated instead.
    pub async fn restart_container(&self, container: &str) -> anyhow::Result<()> {
        self.docker.restart_container(container, Some(RestartContainerOptions { t: Some(10), signal: None })).await?;
        Ok(())
    }

    /// Run a command inside a container, waiting for it to finish.
    pub async fn exec_container(&self, container: &str, cmd: &[&str]) -> anyhow::Result<ExecOutput> {
        let exec = self
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                },
            )
            .await?;

        let started = self
            .docker
            .start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: false, output_capacity: None }))
            .await?;

        let mut output = String::new();
        if let StartExecResults::Attached { output: mut stream, .. } = started {
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                        output.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        Ok(ExecOutput { exit_code, output })
    }

    /// Ensure an orchestrator-managed sidecar container exists, matches the
    /// spec, and is running. Idempotent; recreates on drift (image id, cmd,
    /// env, ports, labels, memory) so config changes and platform upgrades
    /// converge without touching the platform's compose file. Returns true
    /// when the container was (re)created this call.
    pub async fn ensure_sidecar_container(&self, spec: &SidecarSpec) -> anyhow::Result<bool> {
        if let Some(existing) = self.find_container_by_name(&spec.name).await? {
            if self.sidecar_matches(&existing, spec).await {
                // Restart policy covers reboots; this corrects manual stops.
                if !self.is_container_running(&existing).await? {
                    self.docker.start_container(&existing, None::<StartContainerOptions>).await?;
                }
                return Ok(false);
            }
            tracing::info!(container = %spec.name, "sidecar drifted from spec — recreating");
            self.stop_container(&existing).await?;
            self.remove_container(&existing).await?;
        }

        // Pull only when the image is absent locally: self-image sidecars
        // (dns tracks the platform binary) reference local-only images that
        // no registry has, and pinned public images (backup) shouldn't hit
        // registry rate limits on every drift-recreate.
        if self.docker.inspect_image(&spec.image).await.is_err() {
            self.pull_image_with_opts(&spec.image, true).await?;
        }

        let mut labels = HashMap::new();
        labels.insert("litebin.sidecar".to_string(), spec.label.clone());
        labels.insert("com.docker.compose.project".to_string(), SIDECAR_COMPOSE_PROJECT.to_string());
        labels.insert("com.docker.compose.service".to_string(), spec.service.clone());
        labels.insert("com.docker.compose.container-number".to_string(), "1".to_string());
        labels.insert("com.docker.compose.oneoff".to_string(), "False".to_string());

        let mut port_bindings: HashMap<String, Option<Vec<bollard::models::PortBinding>>> = HashMap::new();
        let mut exposed_ports = Vec::new();
        for port in &spec.ports {
            let key = format!("{}/{}", port.container_port, port.protocol);
            port_bindings.entry(key.clone()).or_insert_with(|| {
                exposed_ports.push(key.clone());
                Some(Vec::new())
            });
            port_bindings.get_mut(&key).expect("just inserted").as_mut().expect("just initialized").push(
                bollard::models::PortBinding {
                    host_ip: Some(port.host_ip.clone()),
                    host_port: Some(port.host_port.to_string()),
                },
            );
        }

        let create_body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: Some(spec.cmd.clone()),
            env: (!spec.env.is_empty()).then_some(spec.env.clone()),
            exposed_ports: (!exposed_ports.is_empty()).then_some(exposed_ports),
            host_config: Some(HostConfig {
                binds: Some(spec.binds.clone()),
                // docs/architecture/resource-protection.md
                memory: Some(spec.memory_limit.unwrap_or(256 * 1024 * 1024)),
                memory_reservation: spec.memory_reservation,
                nano_cpus: Some(500_000_000),
                port_bindings: (!port_bindings.is_empty()).then_some(port_bindings),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                log_config: Some(HostConfigLogConfig {
                    typ: Some("json-file".to_string()),
                    config: Some(HashMap::from([
                        ("max-size".to_string(), "10m".to_string()),
                        ("max-file".to_string(), "3".to_string()),
                    ])),
                }),
                network_mode: Some(spec.network.clone()),
                ..Default::default()
            }),
            labels: Some(labels),
            ..Default::default()
        };

        let options = CreateContainerOptions { name: Some(spec.name.clone()), platform: String::new() };
        let response = self.docker.create_container(Some(options), create_body).await?;
        self.docker.start_container(&response.id, None::<StartContainerOptions>).await?;
        tracing::info!(container = %spec.name, image = %spec.image, "created sidecar container");
        Ok(true)
    }

    /// Does the existing container match the spec? Any mismatch (stale image
    /// id, changed cmd/env/ports/labels/caps) triggers a recreate so the
    /// sidecar always converges to the desired state.
    async fn sidecar_matches(&self, id: &str, spec: &SidecarSpec) -> bool {
        let Ok(inspect) = self.docker.inspect_container(id, None).await else {
            return false;
        };
        let Some(config) = inspect.config else { return false };

        // Image: exact id when tracking (self-image sidecars), else the name.
        let image_ok = match &spec.image_id {
            Some(desired_id) => inspect.image.as_deref() == Some(desired_id.as_str()),
            None => config.image.as_deref() == Some(spec.image.as_str()),
        };
        let cmd_ok = match config.cmd.as_deref() {
            Some(cmd) => cmd.iter().eq(spec.cmd.iter()),
            None => spec.cmd.is_empty(),
        };
        let labels = config.labels.unwrap_or_default();
        let labels_ok = labels.get("com.docker.compose.project").is_some_and(|p| p == SIDECAR_COMPOSE_PROJECT)
            && labels.get("litebin.sidecar").is_some_and(|l| l == &spec.label);
        // Every spec env entry must be present verbatim (base image env is fine).
        let env_ok = match config.env.as_deref() {
            Some(env) => spec.env.iter().all(|wanted| env.iter().any(|current| current == wanted)),
            None => spec.env.is_empty(),
        };
        let memory_ok = inspect
            .host_config
            .as_ref()
            .and_then(|host| host.memory)
            .is_some_and(|m| m == spec.memory_limit.unwrap_or(256 * 1024 * 1024));
        let ports_ok = inspect
            .host_config
            .as_ref()
            .and_then(|host| host.port_bindings.as_ref())
            .map(|bindings| {
                let mut desired: HashMap<String, Vec<(String, String)>> = HashMap::new();
                for port in &spec.ports {
                    desired
                        .entry(format!("{}/{}", port.container_port, port.protocol))
                        .or_default()
                        .push((port.host_ip.clone(), port.host_port.to_string()));
                }
                if bindings.len() != desired.len() {
                    return false;
                }
                desired.iter().all(|(key, wanted)| {
                    let Some(Some(current)) = bindings.get(key) else { return false };
                    let mut current: Vec<(String, String)> =
                        current.iter().filter_map(|b| Some((b.host_ip.clone()?, b.host_port.clone()?))).collect();
                    current.sort();
                    let mut wanted = wanted.clone();
                    wanted.sort();
                    current == wanted
                })
            })
            .unwrap_or(spec.ports.is_empty());

        image_ok && cmd_ok && labels_ok && env_ok && memory_ok && ports_ok
    }

    /// Watch a container's logs (following from now), sending each line to `tx`.
    /// Reconnects every 10s when the stream ends (restart/removal).
    pub fn spawn_log_watcher(&self, container: String, tx: tokio::sync::mpsc::UnboundedSender<String>) {
        let docker = self.docker.clone();
        tokio::spawn(async move {
            loop {
                let since = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i32)
                    .unwrap_or(0);
                let options = bollard::query_parameters::LogsOptions {
                    follow: true,
                    stdout: true,
                    stderr: true,
                    since,
                    tail: "0".to_string(),
                    ..Default::default()
                };
                let mut stream = docker.logs(&container, Some(options));
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                            let _ = tx.send(String::from_utf8_lossy(&message).into_owned());
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });
    }

    /// Stop and remove a sidecar container by exact name (idempotent).
    pub async fn remove_sidecar_container(&self, name: &str) -> anyhow::Result<()> {
        if let Some(existing) = self.find_container_by_name(name).await? {
            self.stop_container(&existing).await?;
            self.remove_container(&existing).await?;
            tracing::info!(container = %name, "removed sidecar container");
        }
        Ok(())
    }
}
