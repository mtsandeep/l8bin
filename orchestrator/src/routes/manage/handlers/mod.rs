mod delete;
mod duplicate;
mod export;
mod migrate;
mod recreate;
mod service_ops;
mod shared;
mod stop_start;

pub use delete::*;
pub use duplicate::*;
pub use export::*;
pub use migrate::*;
pub use recreate::*;
pub use service_ops::*;
pub use stop_start::*;

/// Docker volume names a transfer must move separately from the project dir.
/// Binds are excluded — relative binds ride along in the project-dir tar,
/// absolute binds are user-managed host paths.
pub(crate) async fn volume_list(db: &sqlx::SqlitePool, project: &crate::db::models::Project) -> Vec<String> {
    let scoped: Vec<String> = if shared::uses_compose_lifecycle(project.deploy_type.as_ref()) {
        sqlx::query_as::<_, (String,)>(
            "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(db)
        .await
        .map(|rows| rows.into_iter().map(|(name,)| name).collect())
        .unwrap_or_default()
    } else if let Some(ref vols_json) = project.volumes {
        serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(vols_json)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let name = v.name.as_deref().unwrap_or(&project.id);
                if name.starts_with('/') {
                    None
                } else {
                    Some(litebin_common::types::scope_volume_source(name, &project.id))
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    scoped
        .into_iter()
        .filter(|v| litebin_common::types::classify_volume(v) == litebin_common::types::VolumeKind::DockerVolume)
        .collect()
}

/// Compose-declared anonymous volume mounts (`volume_name` NULL rows).
pub(crate) async fn anonymous_mount_count(db: &sqlx::SqlitePool, project_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM project_volumes WHERE project_id = ? AND volume_name IS NULL")
        .bind(project_id)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

/// Absolute bind mounts (`/…` volume rows) — user-managed host paths that
/// transfers never carry.
pub(crate) async fn absolute_bind_count(db: &sqlx::SqlitePool, project_id: &str) -> i64 {
    // Exclude the docker socket — nothing to move.
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_volumes \
         WHERE project_id = ? AND volume_name LIKE '/%' \
         AND volume_name NOT IN ('/var/run/docker.sock', '/run/docker.sock')",
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0)
}

pub(crate) fn anonymous_volume_warning(count: i64) -> String {
    format!("{count} anonymous volume mounts — their data is not carried; name them in compose to persist")
}

pub(crate) fn absolute_bind_warning(count: i64) -> String {
    format!("{count} absolute bind mounts — their data stays on this server; copy the paths manually if needed")
}

/// Raw-ports warning text: pinned host ports when the stored compose is
/// parseable, generic otherwise.
pub(crate) fn raw_ports_warning(project_id: &str) -> String {
    let ports = compose_host_ports(project_id);
    if ports.is_empty() {
        "raw ports enabled — this project binds host ports on its node".to_string()
    } else {
        format!("raw ports enabled — binds host port(s) {} on its node", ports.join(", "))
    }
}

fn compose_host_ports(project_id: &str) -> Vec<String> {
    let yaml = match litebin_common::docker::DockerManager::read_compose(project_id) {
        Some(y) => y,
        None => return Vec::new(),
    };
    let compose = match compose_bollard::ComposeParser::parse(&yaml) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut ports: Vec<String> = Vec::new();
    for svc in compose.services.values() {
        for (_cport, _proto, hport) in svc.port_specs() {
            if let Some(h) = hport
                && !ports.contains(&h.to_string())
            {
                ports.push(h.to_string());
            }
        }
    }
    ports.sort_by_key(|p| p.parse::<u16>().unwrap_or(0));
    ports
}
