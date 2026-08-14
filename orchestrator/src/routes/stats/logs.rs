use axum::{Json, extract::Path, extract::Query, extract::State, http::StatusCode};
use serde_json::json;

use super::super::manage::{agent_base_url, get_node_from_db};
use super::helpers::logs_use_service_selection;
use super::types::{LogsQuery, LogsResponse};
use crate::AppState;
use crate::nodes;

/// GET /projects/:id/logs?tail=100&service=frontend
/// For multi-service projects, `service` selects a specific service's logs.
/// Defaults to the public service if not specified.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/logs",
    params(
        ("project_id" = String, Path, description = "Project ID"),
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
        let base_url = agent_base_url(&state.config, &node);

        let resp = client
            .get(&format!("{}/containers/{}/logs?tail={}", base_url, container_id, tail))
            .send()
            .await
            .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent logs failed: {body}")));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to read log body: {e}")))?;

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

/// GET /projects/:id/deploy-logs — Returns in-memory deploy log lines for a project.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/deploy-logs",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Deploy log lines"),
    ),
    tag = "stats",
    security(("session_auth" = []))
)]
pub async fn deploy_logs(State(state): State<AppState>, Path(project_id): Path<String>) -> Json<serde_json::Value> {
    let lines = state
        .deploy_logs
        .get(&project_id)
        .and_then(|entry| {
            let guard = entry.lock().ok()?;
            Some(guard.clone())
        })
        .unwrap_or_default();

    Json(json!({
        "project_id": project_id,
        "lines": lines,
    }))
}
