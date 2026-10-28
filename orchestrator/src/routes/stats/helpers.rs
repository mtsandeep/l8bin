use litebin_common::types::{DeployType, ProjectStatus, VolumeKind, VolumeMount, classify_volume, scope_volume_source};

use super::UsageEntry;
use super::types::{LiveStats, ServiceInfo, ServiceVolumeInfo, StatsResponse};

const USAGE_TTL_SECS: u64 = 600;

/// Given a map of container_id -> live stats,
/// attach per-service stats to each ServiceInfo and return the final Vec.
/// When running, memory_limit from Docker stats overrides the DB value.
/// cpu_limit from inspect is used if DB value is None.
/// Container IDs in `stopped_cids` are marked as "stopped".
pub(super) fn enrich_services(
    services: &[(ServiceInfo, Option<String>)],
    stats_map: &std::collections::HashMap<String, LiveStats>,
    stopped_cids: &std::collections::HashSet<String>,
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
                    return enriched;
                }
                if let Some(&(cpu, mem_usage, mem_limit, _disk, cpu_limit)) = stats_map.get(container_id) {
                    enriched.cpu_percent = Some(cpu);
                    enriched.memory_usage = Some(mem_usage);
                    enriched.memory_limit_mb = Some((mem_limit / (1024 * 1024)) as i64);
                    if enriched.cpu_limit.is_none() {
                        enriched.cpu_limit = cpu_limit;
                    }
                }
            }
            enriched
        })
        .collect()
}

/// Scoped (volume, service) pairs for a project: project_volumes rows for
/// compose, the projects.volumes JSON column for single-service projects.
pub(super) async fn scoped_volume_list(
    db: &sqlx::SqlitePool,
    project_id: &str,
    fallback_service: Option<&str>,
) -> Vec<(String, String)> {
    let mut volumes: Vec<(String, String)> = sqlx::query_as(
        "SELECT volume_name, service_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if volumes.is_empty()
        && let Some(fallback_service) = fallback_service
    {
        let project: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT deploy_type, volumes FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(db)
                .await
                .unwrap_or(None);
        if let Some((deploy_type, volumes_json)) = project
            && deploy_type.as_deref() != Some("compose")
            && let Some(json) = volumes_json
            && let Ok(mounts) = serde_json::from_str::<Vec<VolumeMount>>(&json)
        {
            volumes = mounts
                .into_iter()
                .map(|m| {
                    let name = m.name.unwrap_or_else(|| project_id.to_string());
                    (scope_volume_source(&name, project_id), fallback_service.to_string())
                })
                .collect();
        }
    }
    volumes
}

/// Per-service disk: `data_gb` = volumes + writable layer, `disk_gb` = that
/// plus image layers (size_root_fs). Cached for 10 minutes.
pub(super) async fn apply_data_usage(
    state: &crate::AppState,
    project_id: &str,
    node_id: Option<&str>,
    services: &mut [(ServiceInfo, Option<String>)],
) {
    let node = node_id.unwrap_or("local");
    let fallback_service = services.first().map(|(svc, _)| svc.service_name.clone());
    let volumes = scoped_volume_list(&state.db, project_id, fallback_service.as_deref()).await;

    let mut per_service: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for (scoped, service_name) in volumes {
        if classify_volume(&scoped) == VolumeKind::AbsoluteBindMount {
            continue;
        }
        if let Some(bytes) = volume_bytes(state, node, &scoped).await {
            *per_service.entry(service_name).or_default() += bytes;
        }
    }

    for (svc, cid) in services.iter_mut() {
        let vol = per_service.get(&svc.service_name).copied().unwrap_or(0);
        if let Some(cid) = cid
            && let Some((rw, root_fs)) = container_disk(state, node, cid).await
        {
            svc.data_gb = Some((vol + rw) as f64 / (1024.0 * 1024.0 * 1024.0));
            svc.disk_gb = Some((vol + root_fs) as f64 / (1024.0 * 1024.0 * 1024.0));
        } else {
            svc.data_gb = Some(vol as f64 / (1024.0 * 1024.0 * 1024.0));
            svc.disk_gb = svc.data_gb;
        }
    }
}

pub(super) async fn volume_bytes(state: &crate::AppState, node: &str, scoped: &str) -> Option<i64> {
    let key = format!("{node}|{scoped}");
    if let Some(entry) = state.usage_cache.get(&key)
        && entry.at.elapsed().as_secs() < USAGE_TTL_SECS
    {
        return Some(entry.bytes);
    }
    // Local: relative binds (`projects/…`) must be mounted by HOST path for du.
    let mount = if node == "local" {
        match scoped.strip_prefix("projects/") {
            Some(rest) => {
                let host = state.docker.projects_host_dir()?;
                format!("{host}/{rest}")
            }
            None => scoped.to_string(),
        }
    } else {
        // Agents resolve `projects/…` scoped names against their own host dir.
        scoped.to_string()
    };
    let bytes = if node == "local" {
        state.docker.volume_usage_bytes(&mount).await.ok()? as i64
    } else {
        crate::nodes::client::AgentClient::resolve(state, node).await.ok()?.volume_usage(&mount).await.ok()? as i64
    };
    state.usage_cache.insert(key, UsageEntry { bytes, at: std::time::Instant::now() });
    Some(bytes)
}

/// (writable layer, rootfs incl. image layers) for a container's host node.
pub(super) async fn container_disk(state: &crate::AppState, node: &str, container_id: &str) -> Option<(i64, i64)> {
    let rw_key = format!("{node}|rw|{container_id}");
    let fs_key = format!("{node}|fs|{container_id}");
    if let (Some(rw), Some(fs)) = (state.usage_cache.get(&rw_key), state.usage_cache.get(&fs_key))
        && rw.at.elapsed().as_secs() < USAGE_TTL_SECS
        && fs.at.elapsed().as_secs() < USAGE_TTL_SECS
    {
        return Some((rw.bytes, fs.bytes));
    }
    let usage = if node == "local" {
        state.docker.disk_usage(container_id).await.ok()?
    } else {
        crate::nodes::client::AgentClient::resolve(state, node).await.ok()?.disk_usage(container_id).await.ok()?
    };
    let now = std::time::Instant::now();
    let rw = usage.size_rw as i64;
    let fs = usage.size_root_fs as i64;
    state.usage_cache.insert(rw_key, UsageEntry { bytes: rw, at: now });
    state.usage_cache.insert(fs_key, UsageEntry { bytes: fs, at: now });
    Some((rw, fs))
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
                data_gb: None,
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
            data_gb: None,
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
    }

    #[test]
    fn one_service_compose_logs_select_the_service_row() {
        assert!(logs_use_service_selection(Some(&DeployType::Compose)));
        assert!(!logs_use_service_selection(Some(&DeployType::Image)));
        assert!(!logs_use_service_selection(None));
    }
}
