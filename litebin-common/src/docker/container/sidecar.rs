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

/// Desired state for an orchestrator-managed sidecar container.
pub struct SidecarSpec {
    pub name: String,
    pub image: String,
    /// Bind specs, e.g. `["myvolume:/data"]`.
    pub binds: Vec<String>,
    pub cmd: Vec<String>,
    pub network: String,
    /// Value for the `litebin.sidecar` label (e.g. `backup`).
    pub label: String,
    /// Compose service label — groups the sidecar with the platform's containers.
    pub service: String,
}

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

    /// The orchestrator container's compose project.
    async fn self_compose_project(&self) -> Option<String> {
        let hostname = std::fs::read_to_string("/etc/hostname").ok()?.trim().to_string();
        let inspect = self.docker.inspect_container(&hostname, None).await.ok()?;
        inspect.config?.labels?.get("com.docker.compose.project").cloned()
    }

    /// Ensure an orchestrator-managed sidecar container exists and is running.
    /// Idempotent — returns true when the container was created this call.
    pub async fn ensure_sidecar_container(&self, spec: &SidecarSpec) -> anyhow::Result<bool> {
        if let Some(existing) = self.find_container_by_name(&spec.name).await? {
            // Restart policy covers reboots; this corrects manual stops.
            let running = self.is_container_running(&existing).await?;
            if !running {
                self.docker.start_container(&existing, None::<StartContainerOptions>).await?;
            }
            return Ok(false);
        }

        self.pull_image_with_opts(&spec.image, true).await?;

        let mut labels = HashMap::new();
        labels.insert("litebin.sidecar".to_string(), spec.label.clone());
        if let Some(project) = self.self_compose_project().await {
            labels.insert("com.docker.compose.project".to_string(), project);
            labels.insert("com.docker.compose.service".to_string(), spec.service.clone());
            labels.insert("com.docker.compose.container-number".to_string(), "1".to_string());
            labels.insert("com.docker.compose.oneoff".to_string(), "False".to_string());
        }

        let create_body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: Some(spec.cmd.clone()),
            host_config: Some(HostConfig {
                binds: Some(spec.binds.clone()),
                // docs/architecture/resource-protection.md
                memory: Some(256 * 1024 * 1024),
                nano_cpus: Some(500_000_000),
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
