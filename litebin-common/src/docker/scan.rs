use std::collections::HashMap;

use bollard::models::MountPointTypeEnum;
use bollard::query_parameters::{ListContainersOptions, RenameContainerOptions};

use crate::types::COMPOSE_FILE_NAMES;

use super::DockerManager;

impl DockerManager {
    /// Rename a running container to a new name (zero-downtime, container keeps running).
    pub async fn rename_container(&self, container_id: &str, new_name: &str) -> anyhow::Result<()> {
        self.docker.rename_container(container_id, RenameContainerOptions { name: new_name.to_string() }).await?;
        tracing::info!(container_id = %container_id, new_name = %new_name, "renamed container");
        Ok(())
    }

    /// Scan all Docker containers on this host and return groups of foreign
    /// (non-LiteBin-managed) containers, ready to be imported.
    ///
    /// Containers are excluded if their name starts with `litebin-`.
    /// Remaining containers are grouped by the `com.docker.compose.project` label;
    /// standalone containers (no label) each form their own single-container group.
    pub async fn scan_foreign_containers(&self) -> anyhow::Result<Vec<crate::scan::ScanGroup>> {
        use crate::scan::{ScanContainer, ScanGroup, ScannedPort, ScannedVolume, is_local_image, sanitize_project_id};
        use crate::types::DeployType;

        // List ALL containers (running + stopped)
        let all = self.docker.list_containers(Some(ListContainersOptions { all: true, ..Default::default() })).await?;

        // Collect container IDs that are not LiteBin-managed
        let foreign_ids: Vec<String> = all
            .into_iter()
            .filter(|c| {
                let names = c.names.as_deref().unwrap_or_default();
                let labels = c.labels.as_ref();
                // Exclude if any name starts with /litebin-
                let is_litebin_name = names.iter().any(|n| n.trim_start_matches('/').starts_with("litebin-"));
                // Exclude if has a litebin label (future-proofing)
                let is_litebin_label = labels.and_then(|l| l.get("litebin.project_id")).is_some();
                !is_litebin_name && !is_litebin_label
            })
            .filter_map(|c| c.id)
            .collect();

        // Inspect each foreign container
        // Group by compose project key (or container name for standalone)
        let mut groups: HashMap<String, Vec<ScanContainer>> = HashMap::new();
        let mut group_meta: HashMap<String, (Option<String>, bool, bool, bool)> = HashMap::new();
        // group_meta: group_key → (compose_working_dir, compose_file_found, env_file_found, is_compose)

        for id in &foreign_ids {
            let inspect = match self.docker.inspect_container(id, None).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(container_id = %id, error = %e, "scan: failed to inspect container, skipping");
                    continue;
                }
            };

            let raw_name = inspect.name.as_deref().unwrap_or("unknown").trim_start_matches('/').to_string();

            let labels = inspect.config.as_ref().and_then(|c| c.labels.as_ref()).cloned().unwrap_or_default();

            let image = inspect.config.as_ref().and_then(|c| c.image.clone()).unwrap_or_default();

            let state_str = inspect
                .state
                .as_ref()
                .and_then(|s| s.status.as_ref())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let compose_project = labels.get("com.docker.compose.project").cloned();
            let service_name = labels.get("com.docker.compose.service").cloned().unwrap_or_else(|| "web".to_string());

            let group_key = compose_project.clone().unwrap_or_else(|| raw_name.clone());

            // Build ports from network_settings
            let ports: Vec<ScannedPort> = inspect
                .network_settings
                .as_ref()
                .and_then(|ns| ns.ports.as_ref())
                .map(|port_map| {
                    let mut result = Vec::new();
                    for (spec, bindings) in port_map {
                        let parts: Vec<&str> = spec.splitn(2, '/').collect();
                        let Ok(internal) = parts[0].parse::<u16>() else { continue };
                        let protocol = parts.get(1).copied().unwrap_or("tcp").to_string();
                        let external = bindings
                            .as_ref()
                            .and_then(|b| b.first())
                            .and_then(|b| b.host_port.as_ref())
                            .and_then(|p| p.parse::<u16>().ok());
                        result.push(ScannedPort { internal, external, protocol });
                    }
                    result.sort_by_key(|p| p.internal);
                    result
                })
                .unwrap_or_default();

            // Build volumes from mounts
            let volumes: Vec<ScannedVolume> = inspect
                .mounts
                .as_ref()
                .map(|mounts| {
                    mounts
                        .iter()
                        .filter_map(|m| {
                            let source = m.source.clone()?;
                            let destination = m.destination.clone()?;
                            let volume_type = match &m.typ {
                                Some(MountPointTypeEnum::BIND) => "bind",
                                Some(MountPointTypeEnum::VOLUME) => "volume",
                                _ => "other",
                            }
                            .to_string();
                            Some(ScannedVolume { source, destination, volume_type })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let has_external_port = ports.iter().any(|p| p.external.is_some());

            // Detect locally-built images:
            // 1. sha256 digest → definitely local
            // 2. compose sets `com.docker.compose.dockerfile` label when `build:` was used
            // 3. image name matches the compose default "<project>-<service>" naming convention
            let has_build_label = labels.contains_key("com.docker.compose.dockerfile");
            let matches_build_pattern = compose_project.as_ref().map_or(false, |proj| {
                // docker-compose names built images as "<project>-<service>" or "<project>_<service>"
                let pat1 = format!("{}-{}", proj, service_name);
                let pat2 = format!("{}_{}", proj, service_name);
                let img_lower = image.to_lowercase();
                let name_part = img_lower.split(':').next().unwrap_or(&img_lower);
                name_part == pat1 || name_part == pat2
            });
            let image_is_local = is_local_image(&image) || has_build_label || matches_build_pattern;

            groups.entry(group_key.clone()).or_default().push(ScanContainer {
                container_id: id.clone(),
                original_name: raw_name,
                service_name,
                image,
                state: state_str,
                ports,
                volumes,
                suggested_public: has_external_port, // refined below
                image_is_local,
            });

            // Track group metadata (only needs to be set once per group)
            group_meta.entry(group_key.clone()).or_insert_with(|| {
                let working_dir = labels.get("com.docker.compose.project.working_dir").cloned();
                let (compose_found, env_found) = if let Some(ref dir) = working_dir {
                    let p = std::path::Path::new(dir);
                    let c = COMPOSE_FILE_NAMES.iter().any(|name| p.join(name).exists());
                    let e = p.join(".env").exists();
                    (c, e)
                } else {
                    (false, false)
                };
                let is_compose = compose_project.is_some();
                (working_dir, compose_found, env_found, is_compose)
            });
        }

        // Build final ScanGroup list
        let mut scan_groups: Vec<ScanGroup> = groups
            .into_iter()
            .map(|(group_key, mut containers)| {
                let (compose_working_dir, compose_file_found, env_file_found, is_compose) =
                    group_meta.remove(&group_key).unwrap_or((None, false, false, false));

                // Refine suggested_public: only the container with the lowest external port
                // gets the flag set to true; clear it on all others.
                let best_idx = containers
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.ports.iter().any(|p| p.external.is_some()))
                    .min_by_key(|(_, c)| {
                        c.ports.iter().filter(|p| p.external.is_some()).map(|p| p.internal).min().unwrap_or(u16::MAX)
                    })
                    .map(|(idx, _)| idx);

                for (idx, c) in containers.iter_mut().enumerate() {
                    c.suggested_public = Some(idx) == best_idx;
                }

                let deploy_type = if is_compose { DeployType::Compose } else { DeployType::Image };

                let suggested_project_id = sanitize_project_id(&group_key);

                ScanGroup {
                    group_key,
                    suggested_project_id,
                    deploy_type,
                    compose_working_dir,
                    compose_file_found,
                    env_file_found,
                    containers,
                }
            })
            .collect();

        // Sort: compose groups first, then standalone; alphabetically within each
        scan_groups.sort_by(|a, b| {
            let a_compose = matches!(a.deploy_type, DeployType::Compose);
            let b_compose = matches!(b.deploy_type, DeployType::Compose);
            b_compose.cmp(&a_compose).then(a.group_key.cmp(&b.group_key))
        });

        Ok(scan_groups)
    }
}
