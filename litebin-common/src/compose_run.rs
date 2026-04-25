use std::collections::HashMap;

use compose_bollard::{BollardMappingOptions, ComposeFile, ComposeParser};

use crate::types::RunServiceConfig;

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

        let configs = build_configs(compose, project_id, extra_env, instance_id, &pub_service_name, &service_order);

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
                bollard_create_body: Some(bollard_config.create_body),
                bollard_host_config: Some(bollard_config.host_config),
            })
        })
        .collect()
}

/// Scope volume names in a compose volume spec with the project ID.
///
/// - `pgdata:/var/lib/postgresql/data` -> `litebin_myproject_pgdata:/var/lib/postgresql/data`
/// - `/host/path:/container/path` -> unchanged (absolute bind mount)
/// - `./data:/container/path` -> `projects/myproject/data:/container/path` (relative to project folder)
fn scope_volume_name(volume_spec: &str, project_id: &str) -> String {
    let (source, rest) = match volume_spec.split_once(':') {
        Some((src, rest)) => (src, format!(":{}", rest)),
        None => return volume_spec.to_string(),
    };
    format!("{}{}", crate::types::scope_volume_source(source, project_id), rest)
}
