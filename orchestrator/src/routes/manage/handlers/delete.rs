use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde_json::json;

use crate::AppState;
use crate::nodes;
use litebin_common::types::NodeStatus;

use crate::routes::manage::helpers::{
    MessageResponse, agent_base_url, cleanup_unused_image, get_node_from_db, sync_caddy,
};
use crate::routes::manage::multi_service::delete_all_services;

use super::shared::uses_compose_lifecycle;

/// Build a list of scoped volume names for a project (from DB for multi-service, from JSON for single-service).
async fn build_volume_list(db: &sqlx::SqlitePool, project: &crate::db::models::Project) -> Vec<String> {
    if uses_compose_lifecycle(project.deploy_type.as_ref()) {
        match sqlx::query_as::<_, (String,)>(
            "SELECT volume_name FROM project_volumes WHERE project_id = ? AND volume_name IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(db)
        .await
        {
            Ok(rows) => rows.into_iter().map(|(name,)| name).collect(),
            Err(e) => {
                tracing::warn!(project = %project.id, error = %e, "delete: failed to fetch project volumes");
                Vec::new()
            }
        }
    } else if let Some(ref vols_json) = project.volumes {
        serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(vols_json)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let name = v.name.as_deref().unwrap_or(&project.id);
                if name.starts_with('/') {
                    None // absolute bind mount — user-managed
                } else {
                    Some(litebin_common::types::scope_volume_source(name, &project.id))
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

/// DELETE /projects/:id
#[utoipa::path(
    delete,
    path = "/projects/{project_id}",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn delete_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    // Remove container(s) — branch on node location
    let is_local = project.node_id.as_deref().map(|n| n == "local").unwrap_or(true);

    if is_local {
        if uses_compose_lifecycle(project.deploy_type.as_ref()) {
            delete_all_services(&state, &project_id).await;
        } else {
            // Collect volumes for single-service local cleanup
            let volumes: Vec<String> = build_volume_list(&state.db, &project).await;
            let _ = state.docker.cleanup_project_resources(&project_id, &volumes).await;
        }
    } else {
        // Remote: best-effort container cleanup via the agent. When the node is
        // offline/destroyed (no client, or status != online) there's nothing to
        // clean up remotely — skip and proceed to DB deletion so the project
        // isn't stuck on a node that no longer exists.
        let node_id = project.node_id.as_deref().unwrap();
        let volumes = build_volume_list(&state.db, &project).await;

        let reachable = match get_node_from_db(&state.db, node_id).await {
            Ok(node) if node.status == NodeStatus::Online => {
                nodes::client::get_node_client(&state.node_clients, node_id).is_ok()
            }
            _ => false,
        };

        if reachable {
            let client = nodes::client::get_node_client(&state.node_clients, node_id)
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
            let node = get_node_from_db(&state.db, node_id).await?;
            let base_url = agent_base_url(&state.config, &node);
            let response = client
                .post(&format!("{}/containers/cleanup", base_url))
                .json(&json!({
                    "project_id": project_id,
                    "volumes": volumes,
                }))
                .send()
                .await
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent cleanup failed: {e}")))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err((StatusCode::BAD_GATEWAY, format!("agent cleanup returned {status}: {body}")));
            }
        } else {
            tracing::warn!(project = %project_id, node_id = node_id, "node unavailable; skipping remote container cleanup and deleting project record");
        }
    }

    // Clean up all per-service images if no longer in use
    let service_images: Vec<String> = sqlx::query_scalar("SELECT image FROM project_services WHERE project_id = ?")
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    let unique_images: std::collections::HashSet<String> = service_images.into_iter().collect();
    for image in &unique_images {
        cleanup_unused_image(&state, project.node_id.as_deref(), image).await;
    }

    // Delete from DB
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;

    // Clear in-memory deploy logs
    crate::routes::deploy::logs::clear_deploy_logs(&state, &project_id);

    // Resync Caddy routes
    sync_caddy(&state).await;

    tracing::info!(project = %project_id, "project deleted via API");

    Ok(Json(MessageResponse { message: format!("project '{}' deleted", project_id), ..Default::default() }))
}
