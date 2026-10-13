use axum::{Json, extract::Path, extract::Query, extract::State, http::StatusCode};

use super::super::manage::get_node_from_db;
use super::helpers::logs_use_service_selection;
use super::types::{DeployLogsResponse, LogsQuery, LogsResponse};
use crate::AppState;
use crate::nodes;

#[utoipa::path(
    get,
    path = "/projects/{id}/logs",
    description = "Tail stored log lines for a project; for multi-service projects, `service` selects a specific service (defaults to the public service).",
    params(
        ("id" = String, Path, description = "Project ID"),
        ("tail" = Option<usize>, Query, description = "Number of log lines to return"),
        ("service" = Option<String>, Query, description = "Service name to filter logs"),
    ),
    responses(
        (status = 200, body = LogsResponse),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn project_logs(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, String)> {
    let project = sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or((StatusCode::NOT_FOUND, format!("project '{}' not found", project_id)))?;

    let tail = query.tail.unwrap_or(100);

    // Resolve the container_id to tail logs from
    let (container_id, service_name) = if logs_use_service_selection(project.deploy_type.as_ref()) {
        // Multi-service: look up specific service or fall back to public service
        if let Some(ref svc) = query.service {
            let row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT container_id FROM project_services WHERE project_id = ? AND service_name = ?")
                    .bind(&project_id)
                    .bind(svc)
                    .fetch_optional(&state.db)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            match row.and_then(|(cid,)| cid) {
                Some(cid) => (cid, Some(svc.clone())),
                None => return Err((StatusCode::NOT_FOUND, format!("service '{}' not found", svc))),
            }
        } else {
            // Default to public service
            let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT container_id, service_name FROM project_services WHERE project_id = ? AND is_public = 1",
            )
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
            match row.and_then(|(cid, name)| cid.map(|c| (c, name))) {
                Some((cid, name)) => (cid, name),
                None => {
                    // No public service — try first service
                    let row2: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                        "SELECT container_id, service_name FROM project_services WHERE project_id = ? AND container_id IS NOT NULL LIMIT 1"
                    )
                    .bind(&project_id)
                    .fetch_optional(&state.db)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?;
                    match row2.and_then(|(cid, name)| cid.map(|c| (c, name))) {
                        Some((cid, name)) => (cid, name),
                        None => return Err((StatusCode::BAD_REQUEST, "no running service containers".to_string())),
                    }
                }
            }
        }
    } else {
        // Single-service
        let cid = project
            .container_id
            .as_deref()
            .ok_or((StatusCode::BAD_REQUEST, "no container id".to_string()))?
            .to_string();
        (cid, None)
    };

    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    let lines = if is_remote {
        let node_id = project.node_id.as_deref().unwrap();
        let client = nodes::client::get_node_client(&state.node_clients, node_id)
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client unavailable: {e}")))?;
        let node = get_node_from_db(&state.db, node_id).await?;
        let agent = nodes::client::AgentClient::new(client, &node, &state.config);

        let body = match agent.container_logs(&container_id, Some(tail)).await {
            Ok(body) => body,
            Err(nodes::client::AgentClientError::Status { body, .. }) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent logs failed: {body}")));
            }
            Err(e) => {
                return Err((StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")));
            }
        };

        body.lines().map(|l| l.to_string()).collect()
    } else {
        state
            .docker
            .container_logs(&container_id, tail)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("logs error: {e}")))?
    };

    Ok(Json(LogsResponse { project_id, service_name, lines }))
}

#[utoipa::path(
    get,
    path = "/projects/{id}/deploy-logs",
    description = "Return the in-memory deploy log lines captured during the project's last deploy.",
    params(
        ("id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, body = DeployLogsResponse),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn deploy_logs(State(state): State<AppState>, Path(project_id): Path<String>) -> Json<DeployLogsResponse> {
    let lines = state
        .deploy_logs
        .get(&project_id)
        .and_then(|entry| {
            let guard = entry.lock().ok()?;
            Some(guard.clone())
        })
        .unwrap_or_default();

    Json(DeployLogsResponse { project_id, lines })
}
