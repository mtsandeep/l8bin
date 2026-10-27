//! Export manifest — the portable DB shape carried inside an export bundle.
//!
//! Everything import needs to recreate a project on another master: durable
//! `projects` fields (custom domain included), service rows, scoped volumes,
//! routes, and the uploaded images carried in the bundle. Instance-specific
//! fields (user, node, container state, status) are deliberately absent.

use serde::{Deserialize, Serialize};

use litebin_common::types::Project;

pub const MANIFEST_FORMAT: &str = "litebin.export";
/// v2: `project.custom_domain` + `routes` (path routes and alias subdomains).
/// Both default-on for parsing, so v1 bundles keep importing.
pub const MANIFEST_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportManifest {
    pub format: String,
    pub version: i64,
    pub created_at: i64,
    pub source: ExportSource,
    pub project: ExportProject,
    pub services: Vec<ExportService>,
    pub volumes: Vec<ExportVolume>,
    /// Uploaded images carried in the bundle (`sha256:` refs only; registry refs pull fresh).
    pub images: Vec<ExportImage>,
    /// Path routes and alias subdomains (claimed on import unless taken).
    #[serde(default)]
    pub routes: Vec<ExportRoute>,
    pub flags: ExportFlags,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportSource {
    pub master_version: String,
    pub node_id: String,
    pub architecture: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportProject {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_background: bool,
    pub image: Option<String>,
    pub internal_port: Option<i64>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub auto_stop_enabled: bool,
    pub auto_stop_timeout_mins: i64,
    pub auto_start_enabled: bool,
    pub allow_raw_ports: bool,
    pub allow_docker_access: bool,
    /// Raw single-service volume mounts (JSON column) — scoping derives at run time.
    pub volumes: Option<String>,
    /// `"image"` | `"compose"`.
    pub deploy_type: Option<String>,
    /// User-owned custom domain; claimed on import unless another project holds it.
    #[serde(default)]
    pub custom_domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportService {
    pub service_name: String,
    pub image: String,
    pub port: Option<i64>,
    pub cmd: Option<String>,
    pub is_public: bool,
    pub depends_on: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub is_oneshot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportVolume {
    /// Scoped name on the source (`litebin_{id}_{name}` / `projects/{id}/…`).
    pub scoped_name: String,
    pub service_name: String,
    pub container_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportImage {
    pub r#ref: String,
    /// Bundle entry path (`images/{slug}.tar`).
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportRoute {
    /// `"path"` | `"alias"`.
    pub route_type: String,
    pub path: Option<String>,
    pub subdomain: Option<String>,
    pub upstream: String,
    pub priority: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExportFlags {
    pub include_env: bool,
    pub include_volumes: bool,
}

/// sqlx row shapes for the manifest's tuple queries.
type ServiceRow = (String, String, Option<i64>, Option<String>, bool, Option<String>, Option<i64>, Option<f64>, bool);
pub(crate) type RouteRow = (String, Option<String>, Option<String>, String, i64);

/// Build a manifest from the DB rows of a live project.
pub async fn build_from_db(
    db: &sqlx::SqlitePool,
    project: &Project,
    source: ExportSource,
    include_env: bool,
    include_volumes: bool,
) -> anyhow::Result<ExportManifest> {
    let services: Vec<ServiceRow> = sqlx::query_as(
        "SELECT service_name, image, port, cmd, is_public, depends_on, memory_limit_mb, cpu_limit, is_oneshot \
         FROM project_services WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_all(db)
    .await?;

    let volumes: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT volume_name, container_path, service_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(&project.id)
    .fetch_all(db)
    .await?;

    let images = super::project_image_refs(db, project)
        .await
        .into_iter()
        .filter(|r| r.starts_with("sha256:"))
        .map(|r| ExportImage { file: image_entry_name(&r), r#ref: r })
        .collect();

    let routes: Vec<RouteRow> = sqlx::query_as(
        "SELECT route_type, path, subdomain, upstream, priority FROM project_routes WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_all(db)
    .await?;
    let routes = routes
        .into_iter()
        .map(|(route_type, path, subdomain, upstream, priority)| ExportRoute {
            route_type,
            path,
            subdomain,
            upstream,
            priority,
        })
        .collect();

    Ok(ExportManifest {
        format: MANIFEST_FORMAT.to_string(),
        version: MANIFEST_VERSION,
        created_at: chrono::Utc::now().timestamp(),
        source,
        project: ExportProject {
            id: project.id.clone(),
            name: project.name.clone(),
            description: project.description.clone(),
            is_background: project.is_background,
            image: project.image.clone(),
            internal_port: project.internal_port,
            cmd: project.cmd.clone(),
            memory_limit_mb: project.memory_limit_mb,
            cpu_limit: project.cpu_limit,
            auto_stop_enabled: project.auto_stop_enabled,
            auto_stop_timeout_mins: project.auto_stop_timeout_mins,
            auto_start_enabled: project.auto_start_enabled,
            allow_raw_ports: project.allow_raw_ports,
            allow_docker_access: project.allow_docker_access,
            volumes: project.volumes.clone(),
            deploy_type: project.deploy_type.as_ref().map(|d| d.to_string()),
            custom_domain: project.custom_domain.clone(),
        },
        services: services
            .into_iter()
            .map(|(service_name, image, port, cmd, is_public, depends_on, memory_limit_mb, cpu_limit, is_oneshot)| {
                ExportService {
                    service_name,
                    image,
                    port,
                    cmd,
                    is_public,
                    depends_on,
                    memory_limit_mb,
                    cpu_limit,
                    is_oneshot,
                }
            })
            .collect(),
        volumes: volumes
            .into_iter()
            .map(|(scoped_name, container_path, service_name)| ExportVolume {
                scoped_name,
                service_name,
                container_path,
            })
            .collect(),
        images,
        routes,
        flags: ExportFlags { include_env, include_volumes },
    })
}

/// Bundle entry name for an uploaded image ref: `images/{slug}.tar` where the
/// slug keeps only `[A-Za-z0-9._-]`.
pub fn image_entry_name(image_ref: &str) -> String {
    let slug: String =
        image_ref.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' }).collect();
    format!("images/{slug}.tar")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let manifest = ExportManifest {
            format: MANIFEST_FORMAT.into(),
            version: MANIFEST_VERSION,
            created_at: 123,
            source: ExportSource {
                master_version: "0.4.14".into(),
                node_id: "local".into(),
                architecture: Some("x86_64".into()),
            },
            project: ExportProject {
                id: "my-app".into(),
                name: Some("My App".into()),
                description: None,
                is_background: false,
                image: None,
                internal_port: Some(3000),
                cmd: None,
                memory_limit_mb: None,
                cpu_limit: None,
                auto_stop_enabled: true,
                auto_stop_timeout_mins: 15,
                auto_start_enabled: false,
                allow_raw_ports: false,
                allow_docker_access: false,
                volumes: None,
                deploy_type: Some("compose".into()),
                custom_domain: Some("app.example.com".into()),
            },
            services: vec![ExportService {
                service_name: "web".into(),
                image: "nginx:1.27".into(),
                port: Some(80),
                cmd: None,
                is_public: true,
                depends_on: None,
                memory_limit_mb: None,
                cpu_limit: None,
                is_oneshot: false,
            }],
            volumes: vec![ExportVolume {
                scoped_name: "litebin_my-app_pgdata".into(),
                service_name: "db".into(),
                container_path: Some("/var/lib/postgresql/data".into()),
            }],
            images: vec![ExportImage { r#ref: "sha256:abcdef".into(), file: image_entry_name("sha256:abcdef") }],
            routes: vec![ExportRoute {
                route_type: "alias".into(),
                path: None,
                subdomain: Some("short".into()),
                upstream: "web:3000".into(),
                priority: 100,
            }],
            flags: ExportFlags { include_env: true, include_volumes: true },
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: ExportManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.project.id, "my-app");
        assert_eq!(back.volumes[0].scoped_name, "litebin_my-app_pgdata");
        assert_eq!(back.images[0].file, "images/sha256-abcdef.tar");
        assert_eq!(back.routes[0].subdomain.as_deref(), Some("short"));
        assert_eq!(back.project.custom_domain.as_deref(), Some("app.example.com"));
        assert!(back.flags.include_env);
    }

    #[test]
    fn image_entry_names_stay_path_safe() {
        assert_eq!(image_entry_name("sha256:abc+/x"), "images/sha256-abc--x.tar");
    }
}
