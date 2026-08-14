use std::collections::HashMap;

use bollard::query_parameters::{ListContainersOptions, LogsOptions};
use futures_util::StreamExt;

use super::super::{DockerManager, RunningContainer};
use crate::types::{container_name, parse_container_name};

pub(crate) fn is_project_workload_container(
    project_id: &str,
    names: &[String],
    labels: Option<&HashMap<String, String>>,
) -> bool {
    let labeled_project = labels.and_then(|labels| labels.get("litebin.project_id"));
    if labeled_project.is_some_and(|value| value != project_id) {
        return false;
    }

    let proxy_name = container_name(project_id, crate::types::DOCKER_PROXY_SERVICE, None);
    let is_proxy = names.iter().any(|name| name.trim_start_matches('/') == proxy_name)
        || labels
            .and_then(|labels| labels.get("com.docker.compose.service"))
            .is_some_and(|service| service == crate::types::DOCKER_PROXY_SERVICE);
    if is_proxy {
        return false;
    }

    if labeled_project.map(String::as_str) == Some(project_id) {
        return true;
    }

    let single_name = container_name(project_id, "web", None);
    let service_prefix = format!("litebin-{project_id}.");
    names.iter().any(|name| {
        let name = name.trim_start_matches('/');
        name == single_name || name.strip_prefix(&service_prefix).is_some_and(|service| !service.is_empty())
    })
}

impl DockerManager {
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

    /// Find a container by its name (e.g. "litebin-myapp") and return its ID.
    /// Returns None if no container with that name exists (in any state).
    pub async fn find_container_by_name(&self, name: &str) -> anyhow::Result<Option<String>> {
        let options = ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([("name".to_string(), vec![name.to_string()])])),
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
        let containers =
            self.docker.list_containers(Some(ListContainersOptions { all: false, ..Default::default() })).await?;
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

    /// List every workload container belonging to a project, including stopped
    /// containers and replacements whose IDs are not known by the orchestrator.
    pub async fn list_project_workload_containers(&self, project_id: &str) -> anyhow::Result<Vec<String>> {
        let containers =
            self.docker.list_containers(Some(ListContainersOptions { all: true, ..Default::default() })).await?;

        Ok(containers
            .into_iter()
            .filter(|container| {
                is_project_workload_container(
                    project_id,
                    container.names.as_deref().unwrap_or_default(),
                    container.labels.as_ref(),
                )
            })
            .filter_map(|container| container.id)
            .collect())
    }

    /// List all running litebin containers. Returns parsed container info using the
    /// centralized naming convention (`litebin-{project_id}`, `litebin-{project_id}-{service}`, etc.).
    pub async fn list_running_litebin_containers(&self) -> anyhow::Result<Vec<RunningContainer>> {
        let options = ListContainersOptions { all: false, ..Default::default() };
        let containers = self.docker.list_containers(Some(options)).await?;

        let mut result = Vec::new();
        for c in containers {
            let names = match &c.names {
                Some(n) => n,
                None => continue,
            };
            for name in names {
                let trimmed = name.trim_start_matches('/');
                if let Some((project_id, service_name, instance_id)) = parse_container_name(trimmed) {
                    // Extract ports from list response
                    let ports = c.ports.as_ref().and_then(|ports| {
                        ports.iter().find_map(|p| match (p.private_port, p.public_port) {
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
    pub async fn container_logs(&self, container_id: &str, tail: usize) -> anyhow::Result<Vec<String>> {
        let opts = LogsOptions { stdout: true, stderr: true, tail: tail.to_string(), ..Default::default() };

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
