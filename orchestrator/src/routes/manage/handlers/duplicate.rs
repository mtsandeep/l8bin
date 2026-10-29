//! Project duplication — copy a project to a new id (same or another node)
//! without touching the source. Shares the transfer engine with migration;
//! the rename cascade (containers, network, volume scope, subdomain) is free
//! because every unique name derives from the project id at run time.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::copy_engine::{self, NodeRef};
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::{Node, Project, ProjectStatus};

use super::stop_start::start_project;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DuplicateRequest {
    /// New project id; defaults to `{id}-copy`. When taken, a numeric suffix is applied.
    pub new_id: Option<String>,
    pub name: Option<String>,
    /// Defaults to the source project's node. Same node is allowed (side-by-side copy).
    pub target_node_id: Option<String>,
    #[serde(default = "default_true")]
    pub copy_volumes: bool,
    #[serde(default = "default_true")]
    pub include_env: bool,
    #[serde(default = "default_true")]
    pub start_after: bool,
}

fn default_true() -> bool {
    true
}

#[utoipa::path(
    post,
    path = "/projects/{id}/duplicate",
    request_body = DuplicateRequest,
    description = "Duplicate a project under a new id: copy project files, volumes, and uploaded images to the target node (same node allowed) and start the copy. The source is never touched.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 202, description = "Duplication started in background", body = serde_json::Value),
        (status = 400, description = "Bad request — invalid id or unknown/offline target node"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "Project is mid-deploy/migrate, or no free id could be allocated"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn duplicate_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<DuplicateRequest>,
) -> impl IntoResponse {
    let project = match sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return err(StatusCode::NOT_FOUND, format!("project '{project_id}' not found")),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
    };

    if matches!(
        project.status,
        ProjectStatus::Deploying
            | ProjectStatus::Migrating
            | ProjectStatus::Exporting
            | ProjectStatus::Stopping
            | ProjectStatus::Importing
    ) {
        return err(StatusCode::CONFLICT, format!("project is {} — wait for it to settle", project.status));
    }

    let source_node_id = project.node_id.clone().unwrap_or_else(|| "local".to_string());
    let target_node_id = payload.target_node_id.clone().unwrap_or_else(|| source_node_id.clone());
    if target_node_id != "local" {
        match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&target_node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) if n.status == litebin_common::types::NodeStatus::Online => {}
            Ok(Some(n)) => return err(StatusCode::BAD_REQUEST, format!("target node '{}' is not online", n.id)),
            Ok(None) => return err(StatusCode::NOT_FOUND, format!("target node '{target_node_id}' not found")),
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
        }
    }

    let mut warnings: Vec<String> = Vec::new();
    let source_ref = node_ref(&source_node_id);
    let target_ref = node_ref(&target_node_id);
    let archs_differ = arch_mismatch(&state, &source_ref, &target_ref).await;
    let image_refs = copy_engine::project_image_refs(&state.db, &project).await;
    if archs_differ {
        if image_refs.iter().any(|r| r.starts_with("sha256:")) {
            return err(
                StatusCode::BAD_REQUEST,
                "source and target architectures differ and the project has uploaded (sha256:) images — \
                 redeploy from source on the target node instead of duplicating"
                    .to_string(),
            );
        }
        warnings.push("source and target architectures differ — registry images must be multi-arch to start".into());
    }
    if target_node_id == source_node_id && project.allow_raw_ports {
        warnings.push(
            "project pins host ports (allow_raw_ports) — the copy may fail to start alongside the original".into(),
        );
    }
    let anon_mounts = super::anonymous_mount_count(&state.db, &project.id).await;
    if anon_mounts > 0 {
        warnings.push(super::anonymous_volume_warning(anon_mounts));
    }

    let base = format!("{}-copy", project.id);
    let reserved = [state.platform.dashboard_subdomain(), state.platform.poke_subdomain()];
    let new_name = payload.name.clone().or_else(|| project.name.clone().map(|n| format!("{n} (copy)")));

    // Allocate + claim the id, retrying with a fresh suffix when a concurrent
    // duplicate wins the INSERT race (unique constraint). The projects INSERT
    // runs first, so a failed attempt leaves no partial rows behind.
    let mut requested = payload.new_id.clone();
    let mut alloc = None;
    for _ in 0..3 {
        let candidate = match copy_engine::allocate_project_id(&state.db, &reserved, requested.as_deref(), &base).await
        {
            Ok(a) => a,
            Err(e) => return err(StatusCode::CONFLICT, e.to_string()),
        };
        if candidate.suffixed {
            let original = requested.clone().unwrap_or_else(|| base.clone());
            warnings.push(format!("'{original}' was taken"));
        }
        match insert_copy_rows(&state, &project, &candidate.id, new_name.as_deref(), &target_node_id, &mut warnings)
            .await
        {
            Ok(()) => {
                alloc = Some(candidate);
                break;
            }
            Err(e) => {
                let raced = e
                    .root_cause()
                    .downcast_ref::<sqlx::Error>()
                    .map(crate::validation::is_unique_constraint)
                    .unwrap_or(false);
                if !raced {
                    return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db insert: {e}"));
                }
                requested = Some(candidate.id);
            }
        }
    }
    let Some(alloc) = alloc else {
        return err(StatusCode::CONFLICT, "id kept colliding — retry the duplication".into());
    };

    let job = DuplicateJob {
        old_id: project.id.clone(),
        new_id: alloc.id.clone(),
        source: source_ref,
        target: target_ref,
        copy_volumes: payload.copy_volumes,
        include_env: payload.include_env,
        start_after: payload.start_after,
    };
    let state_bg = state.clone();
    let new_id = alloc.id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_duplicate(&state_bg, &job).await {
            tracing::error!(project = %new_id, source = %job.old_id, error = %e, "duplication failed");
            crate::routes::deploy::logs::push_deploy_log(&state_bg, &new_id, &format!("duplication failed: {e}"));
            let _ =
                status::transition(&state_bg.db, &new_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await;
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "project_id": alloc.id,
            "status": "importing",
            "warnings": warnings,
        })),
    )
        .into_response()
}

fn err(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn node_ref(node_id: &str) -> NodeRef {
    if node_id == "local" { NodeRef::Local } else { NodeRef::Agent(node_id.to_string()) }
}

async fn arch_mismatch(state: &AppState, source: &NodeRef, target: &NodeRef) -> bool {
    match (source.architecture(state).await, target.architecture(state).await) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

async fn insert_copy_rows(
    state: &AppState,
    source: &Project,
    new_id: &str,
    name: Option<&str>,
    target_node_id: &str,
    warnings: &mut Vec<String>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let node_id_for_db = if target_node_id == "local" { None } else { Some(target_node_id) };

    sqlx::query(
        "INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, \
         status, node_id, cmd, memory_limit_mb, cpu_limit, auto_stop_enabled, auto_stop_timeout_mins, \
         auto_start_enabled, allow_raw_ports, allow_docker_access, volumes, service_count, service_summary, \
         deploy_type, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id)
    .bind(&source.user_id)
    .bind(name)
    .bind(&source.description)
    .bind(source.is_background)
    .bind(&source.image)
    .bind(source.internal_port)
    .bind(ProjectStatus::Importing)
    .bind(node_id_for_db)
    .bind(&source.cmd)
    .bind(source.memory_limit_mb)
    .bind(source.cpu_limit)
    .bind(source.auto_stop_enabled)
    .bind(source.auto_stop_timeout_mins)
    .bind(source.auto_start_enabled)
    .bind(source.allow_raw_ports)
    .bind(source.allow_docker_access)
    .bind(&source.volumes)
    .bind(source.service_count)
    .bind(&source.service_summary)
    .bind(source.deploy_type.clone())
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Services copy verbatim minus runtime state; the start path re-derives containers.
    sqlx::query(
        "INSERT INTO project_services (project_id, service_name, image, port, cmd, is_public, depends_on, \
         memory_limit_mb, cpu_limit, status, is_oneshot) \
         SELECT ?, service_name, image, port, cmd, is_public, depends_on, memory_limit_mb, cpu_limit, 'stopped', is_oneshot \
         FROM project_services WHERE project_id = ?",
    )
    .bind(new_id)
    .bind(&source.id)
    .execute(&state.db)
    .await?;

    // Volumes re-scope to the new id's namespace.
    let volumes: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT volume_name, container_path, service_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
    )
    .bind(&source.id)
    .fetch_all(&state.db)
    .await?;
    for (scoped_name, container_path, service_name) in volumes {
        sqlx::query(
            "INSERT INTO project_volumes (project_id, service_name, volume_name, container_path) VALUES (?, ?, ?, ?)",
        )
        .bind(new_id)
        .bind(service_name)
        .bind(copy_engine::rescope_volume(&scoped_name, &source.id, new_id))
        .bind(container_path)
        .execute(&state.db)
        .await?;
    }

    // Routes copy as-is; alias subdomains that are already claimed are skipped.
    let routes: Vec<crate::copy_engine::manifest::RouteRow> = sqlx::query_as(
        "SELECT route_type, path, subdomain, upstream, priority FROM project_routes WHERE project_id = ?",
    )
    .bind(&source.id)
    .fetch_all(&state.db)
    .await?;
    for (route_type, path, subdomain, upstream, priority) in routes {
        if route_type == "alias"
            && let Some(sub) = subdomain.as_deref()
        {
            // Any holder counts — including the source project, which
            // keeps its aliases; a copy must not double-claim a subdomain.
            let taken: Option<String> = sqlx::query_scalar(
                "SELECT subdomain FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1",
            )
            .bind(sub)
            .fetch_optional(&state.db)
            .await?;
            let project_taken: Option<String> =
                sqlx::query_scalar("SELECT id FROM projects WHERE id = ?").bind(sub).fetch_optional(&state.db).await?;
            if taken.is_some() || project_taken.is_some() {
                warnings.push(format!("alias '{sub}' is taken on this master; skipped"));
                continue;
            }
        }
        sqlx::query(
            "INSERT INTO project_routes (id, project_id, route_type, path, subdomain, upstream, priority, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(new_id)
        .bind(&route_type)
        .bind(path)
        .bind(subdomain)
        .bind(upstream)
        .bind(priority)
        .bind(chrono::Utc::now().timestamp())
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// Everything the background duplication task needs, bundled for run_duplicate.
struct DuplicateJob {
    old_id: String,
    new_id: String,
    source: NodeRef,
    target: NodeRef,
    copy_volumes: bool,
    include_env: bool,
    start_after: bool,
}

async fn run_duplicate(state: &AppState, job: &DuplicateJob) -> anyhow::Result<()> {
    let (old_id, new_id) = (&job.old_id, &job.new_id);
    let (source, target) = (&job.source, &job.target);
    let project =
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?").bind(old_id).fetch_one(&state.db).await?;
    let logs = crate::routes::deploy::logs::push_deploy_log;
    let tmp = copy_engine::spool_dir(state);

    logs(state, new_id, &format!("duplicating from '{old_id}'"));
    copy_engine::transfer_project_dir(state, source, target, old_id, new_id, job.include_env, &tmp).await?;
    logs(state, new_id, "project files copied");

    if job.copy_volumes {
        for volume in super::volume_list(&state.db, &project).await {
            let to = copy_engine::rescope_volume(&volume, old_id, new_id);
            match copy_engine::transfer_volume(state, source, target, &volume, &to).await {
                Ok(()) => logs(state, new_id, &format!("volume '{volume}' copied")),
                Err(e) => {
                    tracing::warn!(project = %new_id, volume = %volume, error = %e, "volume copy skipped");
                    logs(state, new_id, &format!("volume '{volume}' copy skipped: {e}"));
                }
            }
        }
    }

    for image in copy_engine::project_image_refs(&state.db, &project).await {
        if image.starts_with("sha256:") {
            copy_engine::transfer_image(state, source, target, &image, new_id).await?;
            logs(state, new_id, "uploaded image copied");
        }
    }

    status::transition(&state.db, new_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None).await?;
    if job.start_after {
        logs(state, new_id, "starting");
        match start_project(axum::extract::State(state.clone()), axum::extract::Path(new_id.to_string())).await {
            Ok(_) => logs(state, new_id, "started"),
            Err((code, message)) => anyhow::bail!("start failed ({code}): {message}"),
        }
    }

    if target != &NodeRef::Local {
        crate::cloudflare_router::push_project_meta_to_agent(
            target.node_id(),
            &state.db,
            &state.node_clients,
            &state.config,
        )
        .await;
    }
    let _ = state.route_sync_tx.send(());
    logs(state, new_id, "duplication complete");
    Ok(())
}
