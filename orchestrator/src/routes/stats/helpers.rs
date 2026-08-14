use litebin_common::types::{DeployType, ProjectStatus};

use super::types::{LiveStats, ServiceInfo, ServiceVolumeInfo, StatsResponse};

/// Given a map of container_id -> live stats,
/// attach per-service stats to each ServiceInfo and return the final Vec.
/// When running, memory_limit from Docker stats overrides the DB value.
/// cpu_limit from inspect is used if DB value is None.
/// Container IDs in `stopped_cids` are marked as "stopped" with disk from cache.
pub(super) fn enrich_services(
    services: &[(ServiceInfo, Option<String>)],
    stats_map: &std::collections::HashMap<String, LiveStats>,
    stopped_cids: &std::collections::HashSet<String>,
    disk_cache: &dashmap::DashMap<String, i64>,
) -> Vec<ServiceInfo> {
    services
        .iter()
        .map(|(svc, cid)| {
            let mut enriched = svc.clone();
            if let Some(container_id) = cid {
                if stopped_cids.contains(container_id) {
                    if enriched.status != ProjectStatus::Completed {
                        enriched.status = ProjectStatus::Stopped;
                    }
                    enriched.cpu_percent = None;
                    enriched.memory_usage = None;
                    if let Some(bytes) = disk_cache.get(container_id) {
                        enriched.disk_gb = Some(*bytes as f64 / (1024.0 * 1024.0 * 1024.0));
                    }
                    return enriched;
                }
                if let Some(&(cpu, mem_usage, mem_limit, disk, cpu_limit)) = stats_map.get(container_id) {
                    enriched.cpu_percent = Some(cpu);
                    enriched.memory_usage = Some(mem_usage);
                    enriched.memory_limit_mb = Some((mem_limit / (1024 * 1024)) as i64);
                    enriched.disk_gb = Some(disk);
                    if enriched.cpu_limit.is_none() {
                        enriched.cpu_limit = cpu_limit;
                    }
                }
            }
            enriched
        })
        .collect()
}

pub(super) fn select_disk_usage_container_ids(
    deploy_type: Option<&DeployType>,
    project_container_id: Option<&str>,
    service_container_ids: &[Option<String>],
) -> Vec<String> {
    let candidates: Vec<&str> = if deploy_type == Some(&DeployType::Compose) {
        service_container_ids.iter().filter_map(|container_id| container_id.as_deref()).collect()
    } else {
        project_container_id.into_iter().collect()
    };

    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|container_id| !container_id.is_empty())
        .filter(|container_id| seen.insert((*container_id).to_string()))
        .map(str::to_string)
        .collect()
}

pub(super) fn aggregate_root_fs_bytes(sizes: impl IntoIterator<Item = u64>) -> Result<u64, &'static str> {
    sizes.into_iter().try_fold(0_u64, |total, size| total.checked_add(size).ok_or("disk usage total overflowed"))
}

pub(super) fn logs_use_service_selection(deploy_type: Option<&DeployType>) -> bool {
    deploy_type == Some(&DeployType::Compose)
}

pub(super) async fn load_project_container_ids(
    db: &sqlx::SqlitePool,
    project: &crate::db::models::Project,
) -> Result<Vec<String>, sqlx::Error> {
    if project.deploy_type == Some(DeployType::Compose) {
        let services: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT container_id FROM project_services WHERE project_id = ? ORDER BY service_name")
                .bind(&project.id)
                .fetch_all(db)
                .await?;
        let service_container_ids: Vec<Option<String>> =
            services.into_iter().map(|(container_id,)| container_id).collect();
        Ok(select_disk_usage_container_ids(
            project.deploy_type.as_ref(),
            project.container_id.as_deref(),
            &service_container_ids,
        ))
    } else {
        Ok(select_disk_usage_container_ids(project.deploy_type.as_ref(), project.container_id.as_deref(), &[]))
    }
}

/// Collect all container IDs for a project (single or multi-service).
/// Returns (container_ids, is_multi_service).
pub(super) async fn project_container_ids(
    db: &sqlx::SqlitePool,
    project: &crate::db::models::Project,
) -> (Vec<String>, bool) {
    let is_compose = project.deploy_type == Some(DeployType::Compose);
    let ids = load_project_container_ids(db, project).await.unwrap_or_default();
    (ids, is_compose)
}

/// Batch-load services for all given project IDs.
/// Returns a map: project_id -> Vec<(ServiceInfo, Option<container_id>)>.
/// The container_id is used to look up per-service stats.
pub(super) async fn batch_load_services(
    db: &sqlx::SqlitePool,
    project_ids: &[String],
) -> std::collections::HashMap<String, Vec<(ServiceInfo, Option<String>)>> {
    if project_ids.is_empty() {
        return std::collections::HashMap::new();
    }

    // Load from project_services table
    let placeholders = project_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT project_id, service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit FROM project_services WHERE project_id IN ({}) ORDER BY service_name",
        placeholders
    );

    let mut builder = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<i64>,
            Option<i64>,
            bool,
            ProjectStatus,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<f64>,
        ),
    >(&query);
    for pid in project_ids {
        builder = builder.bind(pid);
    }

    let rows = match builder.fetch_all(db).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "stats: failed to fetch service rows");
            Vec::new()
        }
    };

    // Group by project_id
    let mut map: std::collections::HashMap<String, Vec<(ServiceInfo, Option<String>)>> =
        std::collections::HashMap::new();
    for (
        project_id,
        service_name,
        image,
        port,
        mapped_port,
        is_public,
        status,
        container_id,
        cmd,
        memory_limit_mb,
        cpu_limit,
    ) in rows
    {
        map.entry(project_id).or_default().push((
            ServiceInfo {
                service_name,
                image,
                port,
                mapped_port,
                is_public,
                status,
                container_id: container_id.clone(),
                cmd,
                cpu_percent: None,
                memory_usage: None,
                memory_limit_mb,
                cpu_limit,
                disk_gb: None,
                volumes: vec![],
                ports: vec![],
            },
            container_id,
        ));
    }

    // Batch-load volumes for all project_services and attach them
    {
        let vol_placeholders = project_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let vol_query = format!(
            "SELECT project_id, service_name, volume_name, container_path FROM project_volumes WHERE project_id IN ({})",
            vol_placeholders
        );
        let mut vol_builder = sqlx::query_as::<_, (String, String, Option<String>, String)>(&vol_query);
        for pid in project_ids {
            vol_builder = vol_builder.bind(pid);
        }
        let vol_rows = match vol_builder.fetch_all(db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "stats: failed to fetch volume rows");
                Vec::new()
            }
        };
        for (pid, svc_name, vol_name, container_path) in vol_rows {
            if let Some(services) = map.get_mut(&pid) {
                for (svc, _) in services.iter_mut() {
                    if svc.service_name == svc_name {
                        svc.volumes.push(ServiceVolumeInfo { volume_name: vol_name, container_path });
                        break;
                    }
                }
            }
        }
    }

    map
}

pub(super) fn make_stats_response(
    project_id: String,
    status: ProjectStatus,
    last_active_at: Option<i64>,
    services: Vec<ServiceInfo>,
) -> StatsResponse {
    // Completed one-shots do not participate in the runtime aggregate.
    let long_running: Vec<&ServiceInfo> =
        services.iter().filter(|service| service.status != ProjectStatus::Completed).collect();
    let running_count = long_running.iter().filter(|service| service.status == ProjectStatus::Running).count();

    let effective_status = if status != ProjectStatus::Running || services.is_empty() {
        status
    } else if long_running.is_empty() || running_count == 0 {
        ProjectStatus::Stopped
    } else if running_count < long_running.len() {
        ProjectStatus::Degraded
    } else {
        ProjectStatus::Running
    };
    StatsResponse { project_id, status: effective_status.to_string(), last_active_at, services }
}

pub(super) fn inactive_project_status(project_status: ProjectStatus, services: &[ServiceInfo]) -> ProjectStatus {
    if project_status.is_transient() {
        project_status
    } else if services.iter().any(|service| service.status == ProjectStatus::Running) {
        ProjectStatus::Degraded
    } else {
        ProjectStatus::Stopped
    }
}

/// Read each compose project's on-disk `compose.yaml` and collect **every** container
/// port per service. Used to populate `ServiceInfo.ports` for route suggestions (so all
/// of a service's ports are offered, not just the first). Single-image / scan-imported
/// projects have no compose file and are skipped — the dashboard falls back to `port`.
/// Missing/unparseable files are silently ignored.
pub(super) fn compose_service_ports(
    projects: &[crate::db::models::Project],
) -> std::collections::HashMap<String, std::collections::HashMap<String, Vec<i64>>> {
    let mut out: std::collections::HashMap<String, std::collections::HashMap<String, Vec<i64>>> =
        std::collections::HashMap::new();
    for project in projects {
        if project.deploy_type != Some(DeployType::Compose) {
            continue;
        }
        let Some(yaml) = litebin_common::docker::DockerManager::read_compose(&project.id) else {
            continue;
        };
        let Ok(compose) = compose_bollard::ComposeParser::parse(&yaml) else {
            continue;
        };
        let entry = compose
            .services
            .iter()
            .map(|(name, svc)| {
                (
                    name.clone(),
                    litebin_common::compose_run::service_container_ports(&svc.ports)
                        .into_iter()
                        .map(|p| p as i64)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        out.insert(project.id.clone(), entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, status: ProjectStatus) -> ServiceInfo {
        ServiceInfo {
            service_name: name.to_string(),
            image: "test".to_string(),
            port: None,
            mapped_port: None,
            is_public: false,
            status,
            container_id: None,
            cmd: None,
            cpu_percent: None,
            memory_usage: None,
            memory_limit_mb: None,
            cpu_limit: None,
            disk_gb: None,
            volumes: vec![],
            ports: vec![],
        }
    }

    #[test]
    fn completed_oneshot_does_not_keep_stopped_project_degraded() {
        let response = make_stats_response(
            "test".to_string(),
            ProjectStatus::Running,
            None,
            vec![
                service("migration", ProjectStatus::Completed),
                service("web", ProjectStatus::Stopped),
                service("db", ProjectStatus::Stopped),
            ],
        );

        assert_eq!(response.status, "stopped");
    }

    #[test]
    fn stopped_daemon_with_another_running_is_degraded() {
        let response = make_stats_response(
            "test".to_string(),
            ProjectStatus::Running,
            None,
            vec![
                service("migration", ProjectStatus::Completed),
                service("web", ProjectStatus::Running),
                service("db", ProjectStatus::Stopped),
            ],
        );

        assert_eq!(response.status, "degraded");
    }

    #[test]
    fn completed_oneshot_with_running_daemons_is_running() {
        let response = make_stats_response(
            "test".to_string(),
            ProjectStatus::Running,
            None,
            vec![
                service("migration", ProjectStatus::Completed),
                service("web", ProjectStatus::Running),
                service("db", ProjectStatus::Running),
            ],
        );

        assert_eq!(response.status, "running");
    }

    #[test]
    fn completed_oneshot_does_not_degrade_inactive_project() {
        let services = vec![
            service("migration", ProjectStatus::Completed),
            service("web", ProjectStatus::Stopped),
            service("db", ProjectStatus::Stopped),
        ];

        assert_eq!(inactive_project_status(ProjectStatus::Stopped, &services), ProjectStatus::Stopped);
    }

    #[test]
    fn one_service_background_compose_uses_service_container_id() {
        let service_ids = vec![Some("background-worker".to_string())];

        assert_eq!(
            select_disk_usage_container_ids(Some(&DeployType::Compose), None, &service_ids,),
            vec!["background-worker".to_string()]
        );
    }

    #[test]
    fn compose_disk_usage_aggregates_all_current_service_containers() {
        let service_ids = vec![Some("web".to_string()), None, Some("worker".to_string())];
        let ids =
            select_disk_usage_container_ids(Some(&DeployType::Compose), Some("legacy-project-container"), &service_ids);

        assert_eq!(ids, vec!["web".to_string(), "worker".to_string()]);
        assert_eq!(aggregate_root_fs_bytes([2 * 1024_u64, 3 * 1024_u64]), Ok(5 * 1024_u64));
    }

    #[test]
    fn one_service_compose_logs_select_the_service_row() {
        assert!(logs_use_service_selection(Some(&DeployType::Compose)));
        assert!(!logs_use_service_selection(Some(&DeployType::Image)));
        assert!(!logs_use_service_selection(None));
    }
}
