use bollard::query_parameters::ListContainersOptions;

pub use compose_bollard::naming::{bind_source_exposes_docker_socket, is_docker_socket_source};

use crate::types::container_name;

use super::super::DockerManager;

pub(crate) fn sanitize_docker_socket_binds(binds: &[String], is_managed_proxy: bool) -> Vec<String> {
    if is_managed_proxy {
        return binds.to_vec();
    }
    binds
        .iter()
        .filter(|bind| {
            let source = bind.split(':').next().unwrap_or("");
            !bind_source_exposes_docker_socket(source)
        })
        .cloned()
        .collect()
}

impl DockerManager {
    /// Return a healthy, current managed observation proxy and its exact
    /// loopback mapping. A bridged-only proxy legitimately has no mapping.
    pub async fn current_docker_observe_proxy(
        &self,
        project_id: &str,
    ) -> anyhow::Result<Option<(String, Option<u16>)>> {
        let name = container_name(project_id, crate::types::DOCKER_PROXY_SERVICE, None);
        let Some(container_id) = self.find_container_by_name(&name).await? else {
            return Ok(None);
        };
        let inspect = self.docker.inspect_container(&container_id, None).await?;
        let expected_mount_suffix = format!("/projects/{project_id}/docker-observe/haproxy.cfg");
        let has_current_mount = inspect.mounts.as_ref().is_some_and(|mounts| {
            mounts.iter().any(|mount| {
                mount.destination.as_deref() == Some("/usr/local/etc/haproxy/haproxy.cfg")
                    && mount.rw == Some(false)
                    && mount
                        .source
                        .as_deref()
                        .is_some_and(|source| source.replace('\\', "/").ends_with(&expected_mount_suffix))
            })
        });
        let expected_network = crate::types::docker_observe_network_name(project_id, None);
        let has_private_network = inspect
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .is_some_and(|networks| networks.len() == 1 && networks.contains_key(&expected_network));
        let healthy = inspect.state.as_ref().is_some_and(|state| {
            state.running == Some(true)
                && state.health.as_ref().and_then(|health| health.status.as_ref())
                    == Some(&bollard::models::HealthStatusEnum::HEALTHY)
        });
        let config_is_current = std::fs::read_to_string(
            crate::types::projects_dir().join(project_id).join("docker-observe").join("haproxy.cfg"),
        )
        .is_ok_and(|config| config == crate::types::DOCKER_OBSERVE_HAPROXY_CONFIG);
        let image_is_current = inspect.config.as_ref().and_then(|config| config.image.as_deref())
            == Some(crate::types::DOCKER_OBSERVE_PROXY_IMAGE);

        if !(has_current_mount && has_private_network && healthy && config_is_current && image_is_current) {
            return Ok(None);
        }

        let mapped_port = inspect
            .network_settings
            .as_ref()
            .and_then(|settings| settings.ports.as_ref())
            .and_then(|ports| ports.get("2375/tcp"))
            .and_then(|bindings| bindings.as_ref())
            .and_then(|bindings| bindings.first())
            .and_then(|binding| binding.host_port.as_deref())
            .and_then(|port| port.parse().ok());
        Ok(Some((container_id, mapped_port)))
    }

    /// Remove containers left by older unsafe Docker-socket access paths.
    pub async fn cleanup_unsafe_docker_socket_containers(&self) -> anyhow::Result<usize> {
        let containers =
            self.docker.list_containers(Some(ListContainersOptions { all: true, ..Default::default() })).await?;
        let mut removed = 0;

        for container in containers {
            let Some(container_id) = container.id.as_deref() else {
                continue;
            };
            let names = container.names.as_deref().unwrap_or_default();
            let is_proxy_name = names
                .iter()
                .any(|name| name.ends_with(".litebin-docker-proxy") || name.ends_with(".docker-socket-proxy"));
            let is_litebin_workload =
                container.labels.as_ref().is_some_and(|labels| labels.contains_key("litebin.project_id"));
            if !is_proxy_name && !is_litebin_workload {
                continue;
            }

            let inspect = self.docker.inspect_container(container_id, None).await?;
            let exposes_socket = inspect.mounts.as_ref().is_some_and(|mounts| {
                mounts.iter().any(|mount| mount.source.as_deref().is_some_and(bind_source_exposes_docker_socket))
            });
            if !exposes_socket {
                continue;
            }

            let image = inspect.config.as_ref().and_then(|config| config.image.as_deref());
            let managed_name = names
                .iter()
                .find(|name| name.ends_with(".litebin-docker-proxy"))
                .map(|name| name.trim_start_matches('/'));
            let project_id = managed_name.and_then(|name| {
                name.strip_prefix("litebin-").and_then(|name| name.strip_suffix(".litebin-docker-proxy"))
            });
            let has_managed_config_mount = project_id.is_some_and(|project_id| {
                let expected_suffix = format!("/projects/{project_id}/docker-observe/haproxy.cfg");
                inspect.mounts.as_ref().is_some_and(|mounts| {
                    mounts.iter().any(|mount| {
                        mount.destination.as_deref() == Some("/usr/local/etc/haproxy/haproxy.cfg")
                            && mount.rw == Some(false)
                            && mount
                                .source
                                .as_deref()
                                .is_some_and(|source| source.replace('\\', "/").ends_with(&expected_suffix))
                    })
                })
            });
            let has_private_network =
                inspect.network_settings.as_ref().and_then(|settings| settings.networks.as_ref()).is_some_and(
                    |networks| networks.len() == 1 && networks.keys().all(|name| name.ends_with("-docker-observe")),
                );
            let config_is_current = project_id.is_some_and(|project_id| {
                std::fs::read_to_string(
                    crate::types::projects_dir().join(project_id).join("docker-observe").join("haproxy.cfg"),
                )
                .is_ok_and(|config| config == crate::types::DOCKER_OBSERVE_HAPROXY_CONFIG)
            });
            let is_current_proxy = is_proxy_name
                && image == Some(crate::types::DOCKER_OBSERVE_PROXY_IMAGE)
                && has_managed_config_mount
                && has_private_network
                && config_is_current;
            if is_current_proxy {
                continue;
            }

            let _ = self.stop_container(container_id).await;
            self.remove_container(container_id).await?;
            removed += 1;
            tracing::warn!(container_id, ?names, "removed container using an obsolete unsafe Docker socket path");
        }

        Ok(removed)
    }
}
