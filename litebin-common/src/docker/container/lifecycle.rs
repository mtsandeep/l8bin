use std::collections::HashMap;

use bollard::query_parameters::{
    ListContainersOptions, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};

use super::super::DockerErrorKind;
use super::super::DockerManager;
use crate::types::container_name;

use super::should_stabilize_startup;

pub(crate) fn is_idempotent_container_stop_error(error: &bollard::errors::Error) -> bool {
    matches!(error, bollard::errors::Error::DockerResponseServerError { status_code: 304 | 404, .. })
}

pub(crate) fn merge_service_env(
    mut existing: Vec<String>,
    overrides: &[String],
    replace_docker_host: bool,
) -> Vec<String> {
    if replace_docker_host {
        existing.retain(|value| !value.starts_with("DOCKER_HOST="));
    }
    existing.extend(overrides.iter().cloned());
    existing
}

pub(crate) fn project_cleanup_container_prefixes(project_id: &str) -> [String; 2] {
    [format!("litebin-{project_id}."), format!("litebin-{project_id}")]
}

pub(crate) fn project_cleanup_observe_network(project_id: &str) -> String {
    crate::types::docker_observe_network_name(project_id, None)
}

impl DockerManager {
    /// Inspect a container and return the mapped host port.
    /// Returns `None` if no port mapping is found (container may have exited
    /// or port bindings haven't been applied yet).
    pub async fn inspect_mapped_port(&self, container_id: &str) -> anyhow::Result<Option<u16>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let port = info.network_settings.as_ref().and_then(|ns| ns.ports.as_ref()).and_then(|ports| {
            ports.values().find_map(|bindings| {
                bindings.as_ref()?.first().and_then(|b| b.host_port.as_ref().and_then(|p| p.parse::<u16>().ok()))
            })
        });
        Ok(port)
    }

    /// Inspect one exact container port mapping (for example `2375/tcp`).
    pub async fn inspect_mapped_port_for(&self, container_id: &str, port_key: &str) -> anyhow::Result<Option<u16>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        Ok(info
            .network_settings
            .as_ref()
            .and_then(|settings| settings.ports.as_ref())
            .and_then(|ports| ports.get(port_key))
            .and_then(|bindings| bindings.as_ref())
            .and_then(|bindings| bindings.first())
            .and_then(|binding| binding.host_port.as_deref())
            .and_then(|port| port.parse().ok()))
    }

    /// Start an existing stopped service container (preserves port mappings).
    /// Host-network daemons must remain alive through the bounded startup window
    /// before this returns success.
    pub async fn start_existing_container(
        &self,
        container_id: &str,
        service_name: &str,
        is_oneshot: bool,
    ) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "starting existing container");
        let host_network = self.container_uses_host_network(container_id).await?;
        self.docker.start_container(container_id, None::<StartContainerOptions>).await?;
        if should_stabilize_startup(host_network, is_oneshot, false) {
            self.wait_for_host_network_startup(container_id, service_name).await?;
        }
        Ok(())
    }

    pub async fn container_uses_host_network(&self, container_id: &str) -> anyhow::Result<bool> {
        let info = self.docker.inspect_container(container_id, None).await?;
        Ok(info.host_config.as_ref().and_then(|config| config.network_mode.as_deref()) == Some("host"))
    }

    pub async fn stop_container(&self, container_id: &str) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "stopping container");
        if let Err(error) =
            self.docker.stop_container(container_id, Some(StopContainerOptions { t: Some(2), signal: None })).await
        {
            if is_idempotent_container_stop_error(&error) {
                tracing::debug!(container_id = %container_id, "container already stopped or absent");
                return Ok(());
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Stop the current primary container selected by project/service identity.
    /// This is idempotent when the container is absent or already stopped.
    pub async fn stop_primary_service_container(&self, project_id: &str, service_name: &str) -> anyhow::Result<bool> {
        let name = crate::types::primary_service_container_name(project_id, service_name)
            .ok_or_else(|| anyhow::anyhow!("invalid project/service identity"))?;
        let Some(container_id) = self.find_container_by_name(&name).await? else {
            return Ok(false);
        };
        self.stop_container(&container_id).await?;
        Ok(true)
    }

    pub async fn remove_container(&self, container_id: &str) -> anyhow::Result<()> {
        tracing::info!(container_id = %container_id, "removing container");
        self.docker
            .remove_container(container_id, Some(RemoveContainerOptions { force: true, ..Default::default() }))
            .await?;
        Ok(())
    }

    /// Remove container by project name (litebin-<project_id>)
    pub async fn remove_by_name(&self, project_id: &str) -> anyhow::Result<()> {
        let name = format!("litebin-{}", project_id);

        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name]);
        let options = ListContainersOptions { all: true, filters: Some(filters), ..Default::default() };

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
        let options = ListContainersOptions { all: true, filters: Some(filters), ..Default::default() };

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
    pub async fn cleanup_project_resources(&self, project_id: &str, volumes: &[String]) -> anyhow::Result<()> {
        let mut cleanup_errors = Vec::new();
        // 1. Stop + remove all containers matching the project prefix
        let [prefix, single_name] = project_cleanup_container_prefixes(project_id);
        match self.list_containers_by_prefix(&prefix).await {
            Ok(container_ids) => {
                for cid in &container_ids {
                    let _ = self.stop_container(cid).await;
                    if let Err(e) = self.remove_container(cid).await {
                        cleanup_errors.push(format!("remove container {cid}: {e}"));
                    } else {
                        tracing::info!(project = %project_id, container_id = %cid, "cleanup: removed container");
                    }
                }
            }
            Err(e) => cleanup_errors.push(format!("list project containers: {e}")),
        }

        // 2. Also try single-service container name
        match self.list_containers_by_prefix(&single_name).await {
            Ok(single_ids) => {
                for cid in &single_ids {
                    let _ = self.stop_container(cid).await;
                    if let Err(e) = self.remove_container(cid).await
                        && DockerErrorKind::from_anyhow(&e) != DockerErrorKind::NotFound
                    {
                        cleanup_errors.push(format!("remove container {cid}: {e}"));
                    }
                }
            }
            Err(e) => cleanup_errors.push(format!("list single-service container: {e}")),
        }

        // 3. Remove volumes
        for vol_name in volumes {
            if let Err(e) = self.remove_volume_by_name(vol_name).await {
                tracing::warn!(project = %project_id, volume = %vol_name, error = %e, "cleanup: failed to remove volume");
                cleanup_errors.push(format!("remove volume {vol_name}: {e}"));
            }
        }

        // 4. Remove per-project network
        if let Err(e) = self.remove_project_network(project_id, None).await {
            cleanup_errors.push(format!("remove project network: {e}"));
        }
        let observe_network = project_cleanup_observe_network(project_id);
        if let Err(e) = self.remove_named_network(&observe_network).await {
            cleanup_errors.push(format!("remove Docker observation network: {e}"));
        }

        // 5. Remove project directory if it exists
        let project_dir = crate::types::projects_dir().join(project_id);
        if project_dir.is_dir()
            && let Err(e) = std::fs::remove_dir_all(&project_dir)
        {
            tracing::warn!(project = %project_id, error = %e, "cleanup: failed to remove project directory");
            cleanup_errors.push(format!("remove project directory: {e}"));
        }

        if cleanup_errors.is_empty() { Ok(()) } else { anyhow::bail!(cleanup_errors.join("; ")) }
    }

    /// Read the compose.yaml for a project. Returns None if the file doesn't exist.
    pub fn read_compose(project_id: &str) -> Option<String> {
        let path = crate::types::projects_dir().join(project_id).join("compose.yaml");
        std::fs::read_to_string(&path).ok()
    }
}
