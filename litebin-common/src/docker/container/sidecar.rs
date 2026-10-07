use std::collections::HashMap;

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, HostConfigLogConfig, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{CreateContainerOptions, RestartContainerOptions, StartContainerOptions};
use futures_util::StreamExt;

use super::super::DockerManager;

/// Result of an `exec_container` call: (exit code, combined stdout+stderr).
pub struct ExecOutput {
    pub exit_code: i64,
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
}

impl DockerManager {
    /// Find the volume name (or host path, for binds) backing a mount destination on
    /// this container, so a sidecar can share the same storage. Returns None when not
    /// running in a container or the mount is absent (dev mode).
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

    /// Restart a container by id or name. Unlike the recreate-based restart used for
    /// project services, this keeps the container identity (sidecars are not rebuilt).
    pub async fn restart_container(&self, container: &str) -> anyhow::Result<()> {
        self.docker.restart_container(container, Some(RestartContainerOptions { t: Some(10), signal: None })).await?;
        Ok(())
    }

    /// Run a command inside a container and wait for it to finish.
    /// Returns the exit code and combined stdout+stderr output.
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

    /// Ensure an orchestrator-managed sidecar container exists and is running.
    /// Idempotent: pulls the image and creates the container only when missing.
    /// Returns true if the container was created, false if it already existed.
    pub async fn ensure_sidecar_container(&self, spec: &SidecarSpec) -> anyhow::Result<bool> {
        if let Some(existing) = self.find_container_by_name(&spec.name).await? {
            // Exists — make sure it's running (survives host reboots via restart policy,
            // but manual stops are corrected here too).
            let running = self.is_container_running(&existing).await?;
            if !running {
                self.docker.start_container(&existing, None::<StartContainerOptions>).await?;
            }
            return Ok(false);
        }

        self.pull_image_with_opts(&spec.image, true).await?;

        let mut labels = HashMap::new();
        labels.insert("litebin.sidecar".to_string(), spec.label.clone());

        let create_body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: Some(spec.cmd.clone()),
            host_config: Some(HostConfig {
                binds: Some(spec.binds.clone()),
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
