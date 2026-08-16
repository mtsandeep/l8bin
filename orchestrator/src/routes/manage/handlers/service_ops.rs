use std::collections::HashSet;

use axum::{Json, extract::Path, extract::State, http::StatusCode};

use crate::AppState;
use crate::nodes;
use crate::status;

use crate::routes::manage::helpers::{MessageResponse, ensure_node_reachable, sync_caddy};
use crate::routes::manage::multi_service::{
    StartServicesOpts, approved_docker_observe_requesters, proxy_needed_after_stop, start_services, stop_services,
};

use super::recreate::{RecreateRequest, recreate_project};

/// POST /projects/:id/services/:name/start
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/start",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn start_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.node_id.as_deref().is_some_and(|node| node != "local") {
        return recreate_project(
            State(state),
            Path(project_id),
            Some(Json(RecreateRequest { services: Some(vec![service_name]), pull_images: Some(false) })),
        )
        .await;
    }

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    start_services(
        &state,
        &project,
        StartServicesOpts {
            force_recreate: true,
            pull_images: false,
            force_pull: false,
            services: Some(services),
            connect_orchestrator: true,
            rollback_on_failure: false,
        },
    )
    .await?;

    tracing::info!(project = %project_id, service = %service_name, "service started");

    Ok(Json(MessageResponse { message: format!("service '{}' started", service_name), ..Default::default() }))
}

/// POST /projects/:id/services/:name/stop
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/stop",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn stop_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if let Some(node_id) = project.node_id.as_deref().filter(|node| *node != "local") {
        ensure_node_reachable(&state, node_id).await?;
        let requesters = approved_docker_observe_requesters(&state, &project).await?;
        let agent = nodes::client::AgentClient::resolve(&state, node_id)
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        if let Err(e) = agent
            .stop_service(&litebin_common::agent_api::StopServiceRequest {
                project_id: project_id.clone(),
                service_name: service_name.clone(),
            })
            .await
        {
            return Err(match e {
                nodes::client::AgentClientError::Status { body, .. } => {
                    (StatusCode::BAD_GATEWAY, format!("remote service stop failed: {body}"))
                }
                e => e.into_response_parts(),
            });
        }
        status::set_service_stopped(&state.db, &project_id, &service_name)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
        status::derive_and_set_project_status(&state.db, &project_id).await;
        let running_services: HashSet<String> = sqlx::query_scalar(
            "SELECT service_name FROM project_services WHERE project_id = ? AND status IN ('running', 'stopping')",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .into_iter()
        .collect();
        let no_additional_stops = HashSet::new();
        if !proxy_needed_after_stop(&requesters, &running_services, Some(&no_additional_stops)) {
            let proxy_name =
                litebin_common::types::container_name(&project_id, litebin_common::types::DOCKER_PROXY_SERVICE, None);
            if let Err(e) = agent.remove(&litebin_common::agent_api::RemoveRequest { container_id: proxy_name }).await {
                return Err(match e {
                    nodes::client::AgentClientError::Status { body, .. } => {
                        (StatusCode::BAD_GATEWAY, format!("remote Docker observation proxy cleanup failed: {body}"))
                    }
                    e => (StatusCode::SERVICE_UNAVAILABLE, format!("failed to remove Docker observation proxy: {e}")),
                });
            }
        }
    } else {
        let mut services = HashSet::new();
        services.insert(service_name.clone());
        stop_services(&state, &project_id, Some(&services)).await?;
    }

    // Derive project status from aggregate service states
    status::derive_and_set_project_status(&state.db, &project_id).await;

    sync_caddy(&state).await;
    tracing::info!(project = %project_id, service = %service_name, "service stopped");

    Ok(Json(MessageResponse { message: format!("service '{}' stopped", service_name), ..Default::default() }))
}

/// POST /projects/:id/services/:name/restart
#[utoipa::path(
    post,
    path = "/projects/{project_id}/services/{name}/restart",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("name" = String, Path, description = "Service name"),
    ),
    responses(
        (status = 200, body = MessageResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn restart_service(
    State(state): State<AppState>,
    Path((project_id, service_name)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    if project.node_id.as_deref().is_some_and(|node| node != "local") {
        return recreate_project(
            State(state),
            Path(project_id),
            Some(Json(RecreateRequest { services: Some(vec![service_name]), pull_images: Some(false) })),
        )
        .await;
    }

    let mut services = HashSet::new();
    services.insert(service_name.clone());

    // force_recreate handles stop+remove+create, fixing the name conflict bug
    start_services(
        &state,
        &project,
        StartServicesOpts {
            force_recreate: true,
            pull_images: false,
            force_pull: false,
            services: Some(services),
            connect_orchestrator: true,
            rollback_on_failure: false,
        },
    )
    .await?;

    tracing::info!(project = %project_id, service = %service_name, "service restarted");

    Ok(Json(MessageResponse { message: format!("service '{}' restarted", service_name), ..Default::default() }))
}
