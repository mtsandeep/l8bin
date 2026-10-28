//! Project migration across nodes.
//!
//! Stop → transfer project files, volumes, and uploaded images → switch
//! `node_id` → start on the target → clean up the source. Registry images
//! pull fresh on the target; `sha256:` images transfer via docker save/load.
//! Cross-arch `sha256:` moves are rejected.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::docker::TransferKind;
use litebin_common::types::{Node, Project, ProjectStatus};

use super::shared::uses_compose_lifecycle;
use super::stop_start::start_project;
use crate::routes::manage::multi_service::stop_services;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MigrateRequest {
    pub target_node_id: String,
    #[serde(default)]
    pub migrate_volumes: bool,
}

#[utoipa::path(
    post,
    path = "/projects/{id}/migrate",
    request_body = MigrateRequest,
    description = "Migrate a project to another node: stop, transfer project files / volumes / uploaded images, restart on the target, clean up the source.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 202, description = "Migration started in background", body = serde_json::Value),
        (status = 400, description = "Bad request — same node, unknown target, or cross-arch uploaded image"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "Project is mid-deploy or already migrating"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn migrate_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<MigrateRequest>,
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

    let source_node = project.node_id.clone().unwrap_or_else(|| "local".to_string());
    if payload.target_node_id == source_node {
        return err(StatusCode::BAD_REQUEST, format!("project is already on node '{source_node}'"));
    }
    if matches!(
        project.status,
        ProjectStatus::Deploying | ProjectStatus::Migrating | ProjectStatus::Exporting | ProjectStatus::Stopping
    ) {
        return err(StatusCode::CONFLICT, format!("project is {} — wait for it to settle", project.status));
    }

    let target_node: Node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&payload.target_node_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(n)) if n.status == litebin_common::types::NodeStatus::Online => n,
        Ok(Some(n)) => {
            return err(StatusCode::BAD_REQUEST, format!("target node '{}' is not online", n.id));
        }
        Ok(None) => return err(StatusCode::NOT_FOUND, format!("target node '{}' not found", payload.target_node_id)),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
    };

    let source_node: Node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&source_node)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("source node '{source_node}' missing from DB"));
        }
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
    };

    let image_refs = project_image_refs(&project, &state.db).await;
    let uploaded: Vec<&str> = image_refs.iter().map(String::as_str).filter(|r| r.starts_with("sha256:")).collect();
    let archs_differ = arch_mismatch(&source_node, &target_node);
    if archs_differ && !uploaded.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "source and target architectures differ and the project has uploaded (sha256:) images — \
             redeploy from source on the target node instead of migrating"
                .to_string(),
        );
    }

    if let Err(e) =
        status::transition(&state.db, &project_id, ProjectStatus::Migrating, &ProjectUpdateFields::default(), None)
            .await
    {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}"));
    }

    let state = state.clone();
    let project_id_bg = project_id.clone();
    let volumes_flag = payload.migrate_volumes;
    let target_id = target_node.id.clone();
    let source_id = source_node.id.clone();
    let arch_warning = archs_differ;
    let anon_mounts = super::anonymous_mount_count(&state.db, &project_id).await;
    let message = if anon_mounts > 0 {
        format!("migration to '{target_id}' started; {}", super::anonymous_volume_warning(anon_mounts))
    } else {
        format!("migration to '{target_id}' started")
    };
    tokio::spawn(async move {
        if let Err(e) = run_migration(&state, &project_id_bg, &source_id, &target_id, volumes_flag, arch_warning).await
        {
            tracing::error!(project = %project_id_bg, error = %e, "migration failed");
            let _ = status::transition(
                &state.db,
                &project_id_bg,
                ProjectStatus::Error,
                &ProjectUpdateFields::default(),
                None,
            )
            .await;
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "project_id": project_id,
            "message": message,
            "status": "migrating",
        })),
    )
        .into_response()
}

fn err(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn arch_mismatch(source: &Node, target: &Node) -> bool {
    match (source.architecture.as_deref(), target.architecture.as_deref()) {
        (Some(a), Some(b)) => a != b,
        // Unknown architecture — let the start decide.
        _ => false,
    }
}

async fn project_image_refs(project: &Project, db: &sqlx::SqlitePool) -> Vec<String> {
    if uses_compose_lifecycle(project.deploy_type.as_ref()) {
        sqlx::query_scalar("SELECT image FROM project_services WHERE project_id = ? AND image IS NOT NULL")
            .bind(&project.id)
            .fetch_all(db)
            .await
            .unwrap_or_default()
    } else {
        project.image.clone().into_iter().collect()
    }
}

async fn run_migration(
    state: &AppState,
    project_id: &str,
    source_id: &str,
    target_id: &str,
    migrate_volumes: bool,
    arch_warning: bool,
) -> anyhow::Result<()> {
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await?;

    // 1. Stop on the source.
    stop_on_source(state, project_id, source_id).await?;

    // 2. Transfer everything under projects/{id} (compose.yaml, .env, metadata).
    transfer(state, source_id, target_id, &TransferKind::ProjectDir(project_id.to_string())).await?;

    // 3. Transfer volumes.
    if migrate_volumes {
        for volume in super::volume_list(&state.db, &project).await {
            match transfer(state, source_id, target_id, &TransferKind::Volume(volume.clone())).await {
                Ok(()) => tracing::info!(project = %project_id, volume = %volume, "volume transferred"),
                Err(e) => {
                    tracing::warn!(project = %project_id, volume = %volume, error = %e, "volume transfer skipped")
                }
            }
        }
    }

    // 4. Transfer uploaded images (registry refs pull fresh on the target).
    for image in project_image_refs(&project, &state.db).await {
        if image.starts_with("sha256:") {
            transfer(state, source_id, target_id, &TransferKind::Image(image)).await?;
        }
    }

    // 5. Remove source containers and networks. Volumes stay — both nodes
    //    may share a Docker daemon.
    if source_id == "local" {
        if let Err(e) = state.docker.cleanup_project_resources(project_id, &[]).await {
            tracing::warn!(project = %project_id, error = %e, "source cleanup incomplete");
        }
    } else {
        match nodes::client::AgentClient::resolve(state, source_id).await {
            Ok(agent) => {
                if let Err(e) = agent
                    .cleanup(&litebin_common::agent_api::CleanupRequest {
                        project_id: project_id.to_string(),
                        volumes: Vec::new(),
                    })
                    .await
                {
                    tracing::warn!(project = %project_id, error = %e, "source cleanup incomplete");
                }
            }
            Err(e) => tracing::warn!(project = %project_id, error = %e, "source cleanup skipped (node unreachable)"),
        }
    }

    // 6. Switch the node and clear stale container state; start picks it up.
    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE projects SET node_id = ?, container_id = NULL, mapped_port = NULL, status = 'stopped', updated_at = ? WHERE id = ?")
        .bind(target_id)
        .bind(now)
        .bind(project_id)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "UPDATE project_services SET container_id = NULL, mapped_port = NULL, status = 'stopped' WHERE project_id = ?",
    )
    .bind(project_id)
    .execute(&state.db)
    .await?;

    // 7. Start on the target.
    match start_project(axum::extract::State(state.clone()), axum::extract::Path(project_id.to_string())).await {
        Ok(_) => tracing::info!(project = %project_id, target = %target_id, "migration deployed on target"),
        Err((code, message)) => anyhow::bail!("start on target failed ({code}): {message}"),
    }

    // 8. Refresh agent metadata on both nodes and rebuild routes.
    for node_id in [source_id, target_id] {
        if node_id != "local" {
            crate::cloudflare_router::push_project_meta_to_agent(
                node_id,
                &state.db,
                &state.node_clients,
                &state.config,
            )
            .await;
        }
    }
    let _ = state.route_sync_tx.send(());

    if arch_warning {
        tracing::warn!(
            project = %project_id,
            "migration crossed architectures — registry images must be multi-arch to start"
        );
    }
    Ok(())
}

async fn stop_on_source(state: &AppState, project_id: &str, source_id: &str) -> anyhow::Result<()> {
    if source_id == "local" {
        let _ = state.docker.remove_by_name(project_id).await;
        stop_services(state, project_id, None).await.ok();
        Ok(())
    } else {
        let agent = nodes::client::AgentClient::resolve(state, source_id).await?;
        agent
            .stop_project(&litebin_common::agent_api::StopProjectRequest { project_id: project_id.to_string() })
            .await
            .map_err(|e| anyhow::anyhow!("source stop failed: {e}"))?;
        Ok(())
    }
}

/// Stream one transfer payload from source to target. `local` uses this
/// Docker manager; agents go through their transfer endpoints.
async fn transfer(state: &AppState, source_id: &str, target_id: &str, kind: &TransferKind) -> anyhow::Result<()> {
    use futures_util::StreamExt;

    tracing::info!(%source_id, %target_id, payload = %kind.describe(), "transferring");
    let projects_dir = state.docker.projects_host_dir();
    let local_kind = crate::copy_engine::local_volume_kind(state, kind);

    let stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = bytes::Bytes> + Send>> = if source_id == "local" {
        let s = state.docker.transfer_export(&local_kind, projects_dir).await?;
        Box::pin(s.map(|chunk| chunk.unwrap_or_default()))
    } else {
        let agent = nodes::client::AgentClient::resolve(state, source_id).await?;
        let s = agent.transfer_export_stream(kind).await?;
        Box::pin(s.map(|chunk| chunk.unwrap_or_default()))
    };

    if target_id == "local" {
        state.docker.transfer_import(&local_kind, projects_dir, stream).await
    } else {
        let agent = nodes::client::AgentClient::resolve(state, target_id).await?;
        agent.transfer_import(kind, stream).await.map_err(|e| anyhow::anyhow!("{e}"))
    }
}
