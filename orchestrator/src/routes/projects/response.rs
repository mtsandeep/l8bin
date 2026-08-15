use serde::Serialize;

use crate::db::models::Project;

use super::super::stats::{ServiceInfo, ServiceVolumeInfo};
use litebin_common::types::{DeployType, ProjectStatus, VolumeMount, scope_volume_source};

// ── Public Stats (service-level data for the public service) ──────────────────
// Reuses ServiceInfo from stats.rs — public_stats is just one service's info.

/// Project response for the API — project metadata + public_stats.
/// The internal `Project` struct (with all DB columns) is used by backend code;
/// this struct is the API-facing shape.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProjectResponse {
    pub id: String,
    pub user_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_background: bool,
    /// True when deployment artifacts are ready for a staged project to start.
    pub is_staged: bool,
    pub node_id: Option<String>,
    pub status: String,
    pub last_active_at: Option<i64>,
    pub auto_stop_enabled: bool,
    pub auto_stop_timeout_mins: i64,
    pub auto_start_enabled: bool,
    pub allow_raw_ports: bool,
    pub custom_domain: Option<String>,
    pub service_count: Option<i64>,
    pub service_summary: Option<String>,
    pub deploy_type: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub public_stats: Option<ServiceInfo>,
}

/// Build PublicStats for a single-service project (no project_services row).
fn public_stats_from_project(project: &Project) -> Option<ServiceInfo> {
    if project.is_background {
        return None;
    }
    let image = project.image.as_deref()?.to_string();
    if image.is_empty() {
        return None;
    }

    // Parse volumes JSON and convert to ServiceVolumeInfo
    let volumes: Vec<ServiceVolumeInfo> = match &project.volumes {
        Some(json) => serde_json::from_str::<Vec<VolumeMount>>(json)
            .unwrap_or_default()
            .into_iter()
            .map(|v| ServiceVolumeInfo {
                volume_name: v.name.map(|name| scope_volume_source(&name, &project.id)),
                container_path: v.path,
            })
            .collect(),
        None => vec![],
    };

    Some(ServiceInfo {
        service_name: "web".to_string(),
        image,
        port: project.internal_port,
        mapped_port: project.mapped_port,
        is_public: true,
        status: project.status.clone(),
        container_id: project.container_id.clone(),
        cmd: project.cmd.clone(),
        cpu_percent: None,
        memory_usage: None,
        memory_limit_mb: project.memory_limit_mb,
        cpu_limit: project.cpu_limit,
        disk_gb: None,
        volumes,
        ports: vec![],
    })
}

/// Build ProjectResponse from a Project row.
/// Row shape of the public service read from project_services.
type PublicServiceRow = (
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
);

pub(super) async fn to_project_response(project: &Project, db: &sqlx::SqlitePool) -> ProjectResponse {
    let public_stats = if project.deploy_type == Some(DeployType::Compose) {
        // Multi-service: look up the public service from project_services
        let row: Option<PublicServiceRow> = sqlx::query_as(
            "SELECT service_name, image, port, mapped_port, is_public, status, container_id, cmd, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ? AND is_public = 1 LIMIT 1"
        )
        .bind(&project.id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

        match row {
            Some((
                service_name,
                image,
                port,
                mapped_port,
                is_public,
                status,
                container_id,
                _cmd,
                memory_limit_mb,
                cpu_limit,
            )) => {
                // Load volumes for this service from project_volumes
                let vol_rows: Vec<(Option<String>, String)> = match sqlx::query_as(
                    "SELECT volume_name, container_path FROM project_volumes WHERE project_id = ? AND service_name = ?",
                )
                .bind(&project.id)
                .bind(&service_name)
                .fetch_all(db)
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(project_id = %project.id, service = %service_name, error = %e, "failed to load volumes");
                        Vec::new()
                    }
                };

                let volumes: Vec<ServiceVolumeInfo> = vol_rows
                    .into_iter()
                    .map(|(volume_name, container_path)| ServiceVolumeInfo { volume_name, container_path })
                    .collect();

                Some(ServiceInfo {
                    service_name,
                    image,
                    port,
                    mapped_port,
                    is_public,
                    status,
                    container_id,
                    cmd: None, // multi-service uses compose for commands
                    cpu_percent: None,
                    memory_usage: None,
                    memory_limit_mb,
                    cpu_limit,
                    disk_gb: None,
                    volumes,
                    ports: vec![],
                })
            }
            None => None,
        }
    } else {
        // Single-service: build from project row
        public_stats_from_project(project)
    };

    ProjectResponse {
        id: project.id.clone(),
        user_id: project.user_id.clone(),
        name: project.name.clone(),
        description: project.description.clone(),
        is_background: project.is_background,
        is_staged: crate::routes::manage::helpers::project_is_staged(project),
        node_id: project.node_id.clone(),
        status: project.status.to_string(),
        last_active_at: project.last_active_at,
        auto_stop_enabled: project.auto_stop_enabled,
        auto_stop_timeout_mins: project.auto_stop_timeout_mins,
        auto_start_enabled: project.auto_start_enabled,
        allow_raw_ports: project.allow_raw_ports,
        custom_domain: project.custom_domain.clone(),
        service_count: project.service_count,
        service_summary: project.service_summary.clone(),
        deploy_type: project.deploy_type.as_ref().map(|d| d.to_string()),
        created_at: project.created_at,
        updated_at: project.updated_at,
        public_stats,
    }
}
