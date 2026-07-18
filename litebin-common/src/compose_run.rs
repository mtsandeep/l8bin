use std::collections::HashMap;

use bollard::models::{ContainerCreateBody, HealthConfig, HostConfig};
use compose_bollard::{BollardMappingOptions, ComposeFile, ComposeParser};

use crate::types::{is_windows_drive_path, NetworkConfig, RunServiceConfig};

/// Result of building service configs from a compose file.
/// Contains everything needed to deploy/wake a multi-service project.
pub struct ComposeRunPlan {
    /// Service names in topological (dependency) order (flattened).
    pub service_order: Vec<String>,
    /// Service names grouped by topological level for parallel startup.
    /// Services within the same level have no dependencies on each other.
    pub service_levels: Vec<Vec<String>>,
    /// service_name → [(dep_name, condition)] from depends_on.
    /// Conditions: "service_started" (default), "service_healthy", "service_completed_successfully".
    pub dependency_conditions: HashMap<String, Vec<(String, String)>>,
    /// Name of the public-facing service (if any).
    pub pub_service_name: Option<String>,
    /// Per-service RunServiceConfig, aligned with service_order.
    pub configs: Vec<RunServiceConfig>,
}

impl ComposeRunPlan {
    /// Build a `ComposeRunPlan` from a pre-parsed `ComposeFile`.
    /// Use this when you've already parsed/validated the compose (e.g. deploy path).
    pub fn from_compose(
        compose: &ComposeFile,
        project_id: &str,
        extra_env: &[String],
        instance_id: Option<&str>,
    ) -> anyhow::Result<Self> {
        if compose.services.contains_key(crate::types::DOCKER_PROXY_SERVICE) {
            anyhow::bail!(
                "service name '{}' is reserved for LiteBin's managed Docker observation proxy",
                crate::types::DOCKER_PROXY_SERVICE
            );
        }
        let service_levels = compose
            .topological_levels()
            .map_err(|e| anyhow::anyhow!("dependency error: {}", e))?;

        let service_order: Vec<String> = service_levels.iter().flatten().cloned().collect();

        let pub_service_name = compose
            .detect_public_service()
            .map_err(|e| anyhow::anyhow!("public service detection: {}", e))?;

        // Build dependency_conditions map from all services
        let mut dependency_conditions: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (name, service) in &compose.services {
            let conditions = service.dependency_conditions();
            if !conditions.is_empty() {
                dependency_conditions.insert(name.clone(), conditions);
            }
        }

        let oneshot_names = compose.oneshot_service_names();
        let configs = build_configs(
            compose,
            project_id,
            extra_env,
            instance_id,
            &pub_service_name,
            &service_order,
            &oneshot_names,
        );

        Ok(Self {
            service_order,
            service_levels,
            dependency_conditions,
            pub_service_name,
            configs,
        })
    }

    /// Check if a service's healthcheck should be waited for.
    /// Returns true if any service in a later level depends on this service
    /// with condition "service_healthy".
    pub fn needs_healthy_wait(&self, svc_name: &str) -> bool {
        for conditions in self.dependency_conditions.values() {
            for (dep, cond) in conditions {
                if dep == svc_name && cond == "service_healthy" {
                    return true;
                }
            }
        }
        false
    }

    /// Check if dependents wait for this service to exit successfully.
    pub fn needs_completed_wait(&self, svc_name: &str) -> bool {
        for conditions in self.dependency_conditions.values() {
            for (dep, cond) in conditions {
                if dep == svc_name && cond == "service_completed_successfully" {
                    return true;
                }
            }
        }
        false
    }

    /// Replace Docker socket declarations with access to a read-only observation proxy.
    /// Only services that declared a Docker socket receive `DOCKER_HOST`.
    pub fn inject_docker_observe_proxy(&mut self, project_id: &str) -> anyhow::Result<bool> {
        let proxy_name = crate::types::DOCKER_PROXY_SERVICE.to_string();
        if !self.configs.iter().any(config_requests_docker_socket) {
            return Ok(false);
        }
        let config_dir = std::path::PathBuf::from("projects")
            .join(project_id)
            .join("docker-observe");
        std::fs::create_dir_all(&config_dir)?;
        let proxy_config_path = config_dir.join("haproxy.cfg");
        std::fs::write(
            &proxy_config_path,
            crate::types::DOCKER_OBSERVE_HAPROXY_CONFIG,
        )?;
        let instance_id = self.configs.first().and_then(|config| config.instance_id.clone());
        let project_network =
            crate::types::project_network_name(project_id, instance_id.as_deref());
        let observe_network =
            crate::types::docker_observe_network_name(project_id, instance_id.as_deref());

        for config in &mut self.configs {
            if config_requests_docker_socket(config) {
                config.docker_observe = true;
                config.env.retain(|value| !value.starts_with("DOCKER_HOST="));
                config.env.push(format!("DOCKER_HOST=tcp://{}:2375", proxy_name));
                config.networks = Some(vec![
                    NetworkConfig {
                        name: project_network.clone(),
                        aliases: Some(vec![config.service_name.clone()]),
                    },
                    NetworkConfig {
                        name: observe_network.clone(),
                        aliases: None,
                    },
                ]);
            }
        }

        // Build a minimal bollard config to trigger the compose path in
        // run_service_container(), which connects to the per-project network.
        let create_body = ContainerCreateBody {
            image: Some(crate::types::DOCKER_OBSERVE_PROXY_IMAGE.to_string()),
            user: Some("0:0".to_string()),
            healthcheck: Some(HealthConfig {
                test: Some(vec![
                    "CMD".into(),
                    "wget".into(),
                    "-q".into(),
                    "-O".into(),
                    "-".into(),
                    "http://127.0.0.1:2375/_ping".into(),
                ]),
                interval: Some(1_000_000_000),
                timeout: Some(1_000_000_000),
                retries: Some(15),
                start_period: Some(2_000_000_000),
                ..Default::default()
            }),
            labels: Some(std::collections::HashMap::from([
                ("com.docker.compose.project".into(), project_id.into()),
                ("com.docker.compose.service".into(), crate::types::DOCKER_PROXY_SERVICE.into()),
            ])),
            ..Default::default()
        };
        let host_config = HostConfig {
            ..Default::default()
        };

        let proxy_config = RunServiceConfig {
            project_id: project_id.to_string(),
            service_name: proxy_name.clone(),
            instance_id,
            image: crate::types::DOCKER_OBSERVE_PROXY_IMAGE.to_string(),
            port: None,
            cmd: None,
            entrypoint: None,
            working_dir: None,
            user: None,
            env: vec![],
            memory_limit_mb: None,
            cpu_limit: None,
            shm_size: None,
            tmpfs: None,
            read_only: None,
            extra_hosts: None,
            networks: Some(vec![NetworkConfig {
                name: observe_network,
                aliases: Some(vec![proxy_name.clone()]),
            }]),
            binds: Some(vec![
                "/var/run/docker.sock:/var/run/docker.sock".to_string(),
                format!(
                    "projects/{project_id}/docker-observe/haproxy.cfg:/usr/local/etc/haproxy/haproxy.cfg:ro"
                ),
            ]),
            is_public: false,
            is_oneshot: false,
            bollard_create_body: Some(create_body),
            bollard_host_config: Some(host_config),
            allow_raw_ports: false,
            docker_observe: true,
            is_managed_docker_proxy: true,
        };

        // Insert at the beginning of service_order and into its own topological
        // level (level 0) so the proxy starts and becomes network-ready before
        // any real services that might need docker.sock.
        self.service_order.insert(0, proxy_name.clone());
        self.service_levels.insert(0, vec![proxy_name.clone()]);
        self.configs.insert(0, proxy_config);
        Ok(true)
    }

    /// Build a minimal `ComposeRunPlan` for a single-service project.
    /// Used when no compose.yaml exists (single-service projects use
    /// `RunServiceConfig::from_project()` to build the config).
    pub fn single_service(config: RunServiceConfig) -> Self {
        let name = config.service_name.clone();
        Self {
            service_order: vec![name.clone()],
            service_levels: vec![vec![name.clone()]],
            dependency_conditions: HashMap::new(),
            pub_service_name: Some(name),
            configs: vec![config],
        }
    }
}

/// Build a `ComposeRunPlan` from compose YAML string.
/// Parses with variable interpolation, validates, and builds configs in one step.
/// Use this when you just need the plan (e.g. agent wake, batch-run).
pub fn build_compose_run_plan(
    compose_yaml: &str,
    project_id: &str,
    extra_env: &[String],
    instance_id: Option<&str>,
) -> anyhow::Result<ComposeRunPlan> {
    let compose = ComposeParser::parse_with_interpolation(compose_yaml, extra_env)
        .map_err(|e| anyhow::anyhow!("invalid compose: {}", e))?;

    ComposeRunPlan::from_compose(&compose, project_id, extra_env, instance_id)
}

fn build_configs(
    compose: &ComposeFile,
    project_id: &str,
    extra_env: &[String],
    instance_id: Option<&str>,
    pub_service_name: &Option<String>,
    service_order: &[String],
    oneshot_names: &std::collections::HashSet<String>,
) -> Vec<RunServiceConfig> {
    let env_map: HashMap<String, String> = extra_env
        .iter()
        .filter_map(|s| {
            let mut parts = s.splitn(2, '=');
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect();

    let options = BollardMappingOptions {
        env_overrides: env_map,
        auto_tmpfs_for_readonly: true,
    };

    service_order
        .iter()
        .filter_map(|svc_name| {
            let svc = compose.services.get(svc_name)?;
            let image = svc.image.clone()?;

            let is_public = pub_service_name.as_deref() == Some(svc_name.as_str());
            let is_oneshot = oneshot_names.contains(svc_name);

            let port: Option<u16> = svc.ports.as_ref()
                .and_then(|p| p.first())
                .and_then(|p| p.split(':').last()?.parse().ok());

            let bollard_config = svc.to_bollard_config(&options);

            let memory_limit_mb: Option<i64> = svc.memory_bytes()
                .map(|bytes| (bytes / (1024 * 1024)) as i64);
            let cpu_limit: Option<f64> = svc.cpus.as_ref()
                .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())));

            // Scope named volumes to the project (e.g. "pgdata" -> "myproject_pgdata").
            // Only prefix volumes whose source doesn't start with / or ./ (those are
            // host bind mounts, not Docker named volumes).
            let binds = svc.volumes.as_ref().map(|vols| {
                vols.iter().map(|v| scope_volume_name(v, project_id)).collect::<Vec<_>>()
            });

            Some(RunServiceConfig {
                project_id: project_id.to_string(),
                service_name: svc_name.clone(),
                instance_id: instance_id.map(|s| s.to_string()),
                image,
                port,
                cmd: None,
                entrypoint: None,
                working_dir: svc.working_dir.clone(),
                user: svc.user.clone(),
                env: extra_env.to_vec(),
                memory_limit_mb,
                cpu_limit,
                shm_size: None,
                tmpfs: None,
                read_only: None,
                extra_hosts: None,
                networks: None,
                binds,
                is_public,
                is_oneshot,
                bollard_create_body: Some(bollard_config.create_body),
                bollard_host_config: Some(bollard_config.host_config),
                allow_raw_ports: false,
                docker_observe: false,
                is_managed_docker_proxy: false,
            })
        })
        .collect()
}

fn config_requests_docker_socket(config: &RunServiceConfig) -> bool {
    config.binds.as_ref().is_some_and(|binds| {
        binds.iter().any(|bind| {
            bind.split(':')
                .next()
                .is_some_and(crate::docker::is_docker_socket_source)
        })
    })
}

/// Scope volume names in a compose volume spec with the project ID.
///
/// - `pgdata:/var/lib/postgresql/data` -> `litebin_myproject_pgdata:/var/lib/postgresql/data`
/// - `/host/path:/container/path` -> unchanged (absolute bind mount)
/// - `./data:/container/path` -> `projects/myproject/data:/container/path` (bind mount under project dir)
/// - `D:/host/path:/container/path` -> `/d/host/path:/container/path` (Windows path
///   converted to MSYS-style so the colon doesn't conflict with Docker's bind separator)
fn scope_volume_name(volume_spec: &str, project_id: &str) -> String {
    // Windows drive-letter paths like "D:/foo:/bar" — the first colon is the drive
    // separator, not the mount separator.  Convert to /d/... format to avoid
    // colon conflicts in Docker's bind mount parser.
    let (source, rest) = if is_windows_drive_path(volume_spec) {
        // Convert "D:/foo" to "/d/foo" (MSYS-style) to eliminate the drive colon
        let drive = volume_spec.as_bytes()[0];
        let sep = if volume_spec.as_bytes()[2] == b'\\' { '/' } else { volume_spec.as_bytes()[2] as char };
        let converted = format!("/{}{}{}", drive as char, sep, &volume_spec[3..]);
        // Find the mount separator in the converted path
        match converted.split_once(':') {
            Some((src, rest)) => (src.to_string(), rest.to_string()),
            None => return converted,
        }
    } else {
        match volume_spec.split_once(':') {
            Some((src, rest)) => (src.to_string(), rest.to_string()),
            None => return volume_spec.to_string(),
        }
    };

    format!("{}:{}", crate::types::scope_volume_source(&source, project_id), rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_observe_is_injected_only_into_socket_requesters() {
        let project_id = format!(
            "observe-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let yaml = r#"
services:
  observer:
    image: example/observer
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
  app:
    image: example/app
"#;
        let mut plan = build_compose_run_plan(yaml, &project_id, &[], None).unwrap();
        plan.inject_docker_observe_proxy(&project_id).unwrap();

        let observer = plan.configs.iter().find(|c| c.service_name == "observer").unwrap();
        let app = plan.configs.iter().find(|c| c.service_name == "app").unwrap();
        let proxy = plan
            .configs
            .iter()
            .find(|c| c.service_name == crate::types::DOCKER_PROXY_SERVICE)
            .unwrap();

        assert!(observer.env.iter().any(|v| {
            v == &format!(
                "DOCKER_HOST=tcp://{}:2375",
                crate::types::DOCKER_PROXY_SERVICE
            )
        }));
        assert!(!app.env.iter().any(|v| v.starts_with("DOCKER_HOST=")));
        let observe_network = crate::types::docker_observe_network_name(&project_id, None);
        assert!(observer
            .networks
            .as_ref()
            .unwrap()
            .iter()
            .any(|network| network.name == observe_network));
        assert!(app.networks.is_none());
        assert_eq!(proxy.networks.as_ref().unwrap().len(), 1);
        assert_eq!(proxy.networks.as_ref().unwrap()[0].name, observe_network);
        assert!(proxy.is_managed_docker_proxy);
        assert!(proxy
            .binds
            .as_ref()
            .unwrap()
            .iter()
            .any(|v| v.starts_with("/var/run/docker.sock:")));

        let generated = std::fs::read_to_string(
            std::path::PathBuf::from("projects")
                .join(&project_id)
                .join("docker-observe")
                .join("haproxy.cfg"),
        )
        .unwrap();
        assert!(generated.contains("acl read_method method GET HEAD"));
        assert!(generated.contains("containers/[^/]+/(json|stats|logs)"));
        assert!(!generated.contains("/exec"));
        assert!(!generated.contains("/archive"));
        assert!(generated.contains("deny_status 403 unless read_method"));
        assert!(generated.contains("deny_status 403 unless observe_endpoint"));

        let _ = std::fs::remove_dir_all(std::path::PathBuf::from("projects").join(project_id));
    }

    #[test]
    fn docker_observe_grant_without_socket_request_does_not_start_proxy() {
        let yaml = "services:\n  app:\n    image: example/app\n";
        let mut plan = build_compose_run_plan(yaml, "observe-unused", &[], None).unwrap();
        assert!(!plan.inject_docker_observe_proxy("observe-unused").unwrap());
        assert!(!plan
            .service_order
            .iter()
            .any(|name| name == crate::types::DOCKER_PROXY_SERVICE));
    }

    #[test]
    fn reserved_proxy_service_name_is_rejected_by_run_plan() {
        let yaml = "services:\n  litebin-docker-proxy:\n    image: attacker/image\n";
        let error = match build_compose_run_plan(yaml, "reserved-name", &[], None) {
            Ok(_) => panic!("reserved proxy service name was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("reserved"));
    }

    #[test]
    fn single_image_socket_request_uses_observation_proxy() {
        let project = crate::types::Project {
            id: "single-observer".into(),
            user_id: "test".into(),
            name: None,
            description: None,
            is_background: true,
            image: Some("example/observer".into()),
            internal_port: None,
            mapped_port: None,
            container_id: None,
            node_id: None,
            status: crate::types::ProjectStatus::Stopped,
            cmd: None,
            memory_limit_mb: None,
            cpu_limit: None,
            custom_domain: None,
            volumes: crate::types::serialize_volumes(&[crate::types::VolumeMount {
                path: "/var/run/docker.sock".into(),
                name: Some("/var/run/docker.sock".into()),
            }]),
            auto_stop_enabled: false,
            auto_stop_timeout_mins: 0,
            auto_start_enabled: false,
            allow_raw_ports: false,
            allow_docker_access: false,
            last_active_at: None,
            service_count: None,
            service_summary: None,
            deploy_type: Some(crate::types::DeployType::Image),
            created_at: 0,
            updated_at: 0,
        };
        let config = crate::types::RunServiceConfig::from_project(&project, Vec::new());
        let mut plan = ComposeRunPlan::single_service(config);
        assert!(plan
            .inject_docker_observe_proxy(&project.id)
            .unwrap());
        let workload = plan
            .configs
            .iter()
            .find(|config| config.service_name == "web")
            .unwrap();
        assert!(workload.docker_observe);
        assert_eq!(workload.networks.as_ref().unwrap().len(), 2);
        assert!(workload
            .env
            .iter()
            .any(|value| value == "DOCKER_HOST=tcp://litebin-docker-proxy:2375"));
        let _ = std::fs::remove_dir_all(
            std::path::PathBuf::from("projects").join(&project.id),
        );
    }

    #[tokio::test]
    #[ignore = "requires a local Docker daemon and pulls haproxy"]
    async fn live_haproxy_allows_observation_and_denies_mutation() {
        let project_id = format!(
            "haproxy-policy-{}",
            std::process::id()
        );
        let yaml = r#"
services:
  observer:
    image: alpine:3.20
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
"#;
        let mut plan = build_compose_run_plan(yaml, &project_id, &[], None).unwrap();
        assert!(plan.inject_docker_observe_proxy(&project_id).unwrap());
        let proxy = plan
            .configs
            .iter_mut()
            .find(|config| config.is_managed_docker_proxy)
            .unwrap();
        let config_path = std::fs::canonicalize(
            std::path::PathBuf::from("projects")
                .join(&project_id)
                .join("docker-observe")
                .join("haproxy.cfg"),
        )
        .unwrap();
        let config_path = config_path
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        proxy.binds.as_mut().unwrap()[1] = format!(
            "{config_path}:/usr/local/etc/haproxy/haproxy.cfg:ro"
        );
        proxy.is_public = true;
        proxy.port = Some(2375);

        let mut docker = crate::docker::DockerManager::new(
            "bridge".into(),
            128 * 1024 * 1024,
            0.25,
        )
        .unwrap();
        docker.detect_host_projects_dir().await;
        docker
            .pull_image_with_opts(crate::types::DOCKER_OBSERVE_PROXY_IMAGE, false)
            .await
            .unwrap();
        docker.ensure_project_network(&project_id, None).await.unwrap();
        let observe_network =
            crate::types::docker_observe_network_name(&project_id, None);
        docker.ensure_named_network(&observe_network).await.unwrap();

        let (container_id, port) = docker.run_service_container(proxy).await.unwrap();
        let policy_result: anyhow::Result<_> = async {
            docker.wait_for_healthy(&container_id, true).await?;
            let client = reqwest::Client::new();
            let base = format!("http://127.0.0.1:{port}");
            let ping = client.get(format!("{base}/_ping")).send().await?;
            let version = client.get(format!("{base}/version")).send().await?;
            let mutation = client
                .post(format!("{base}/containers/create"))
                .send()
                .await?;
            let delete = client
                .delete(format!("{base}/containers/not-real"))
                .send()
                .await?;
            let unlisted = client
                .get(format!("{base}/images/json"))
                .send()
                .await?;
            Ok((
                ping.status(),
                version.status(),
                mutation.status(),
                delete.status(),
                unlisted.status(),
            ))
        }
        .await;

        let _ = docker.stop_container(&container_id).await;
        let _ = docker.remove_container(&container_id).await;
        let _ = docker.remove_named_network(&observe_network).await;
        let _ = docker.remove_project_network(&project_id, None).await;
        let _ = std::fs::remove_dir_all(
            std::path::PathBuf::from("projects").join(&project_id),
        );

        let (ping, version, mutation, delete, unlisted) = policy_result.unwrap();
        assert!(ping.is_success());
        assert!(version.is_success());
        assert_eq!(mutation, reqwest::StatusCode::FORBIDDEN);
        assert_eq!(delete, reqwest::StatusCode::FORBIDDEN);
        assert_eq!(unlisted, reqwest::StatusCode::FORBIDDEN);
    }
}
