use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;

use crate::AppState;
use litebin_common::types::{HealthReport, ImageStats, Node};

use super::ErrorResponse;

#[derive(Serialize, utoipa::ToSchema)]
pub struct NodeImageStatsResponse {
    pub node_id: String,
    pub node_name: String,
    pub image_stats: ImageStats,
}

#[utoipa::path(
    get,
    path = "/nodes/image-stats",
    responses(
        (status = 200, description = "Image statistics per node", body = Vec<NodeImageStatsResponse>),
        (status = 500, description = "Internal server error"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
/// GET /nodes/image-stats — returns image statistics for each node.
pub async fn node_image_stats(State(state): State<AppState>) -> impl IntoResponse {
    let mut results = Vec::new();

    // Local node
    let stats = state.docker.image_stats().await;
    let name = sqlx::query_scalar::<_, String>("SELECT name FROM nodes WHERE id = 'local'")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "local".to_string());

    results.push(NodeImageStatsResponse { node_id: "local".to_string(), node_name: name, image_stats: stats });

    // Remote nodes: query online nodes only (offline ones have no agent and would
    // hang on connect), concurrently with a short timeout so one slow node stalls
    // at most 5s.
    let nodes =
        match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id != 'local' AND status = 'online' ORDER BY name")
            .fetch_all(&state.db)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::warn!(error = %e, "failed to fetch remote nodes for image stats");
                return (StatusCode::OK, Json(results)).into_response();
            }
        };

    let fetches = nodes.iter().map(|node| {
        let node_id = node.id.clone();
        let node_name = node.name.clone();
        let node_clients = state.node_clients.clone();
        let config = state.config.clone();
        async move {
            let client = crate::nodes::client::get_node_client(&node_clients, &node_id).ok()?;
            let base_url = crate::routes::manage::agent_base_url(&config, node);
            let resp = client
                .get(format!("{}/health", base_url))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                tracing::warn!(node_id = %node_id, status = %resp.status(), "agent returned non-success for image stats");
                return None;
            }
            let health = resp.json::<HealthReport>().await.ok()?;
            Some(NodeImageStatsResponse { node_id, node_name, image_stats: health.image_stats })
        }
    });

    for resp in futures_util::future::join_all(fetches).await {
        if let Some(r) = resp {
            results.push(r);
        }
    }

    (StatusCode::OK, Json(results)).into_response()
}

#[utoipa::path(
    post,
    path = "/nodes/{id}/images/prune",
    params(
        ("id" = String, Path, description = "Node ID"),
    ),
    responses(
        (status = 200, description = "Images pruned"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
/// POST /nodes/{id}/images/prune — prune dangling images on a specific node.
pub async fn prune_node_images(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    if id == "local" {
        match state.docker.prune_dangling_images().await {
            Ok(reclaimed) => {
                (StatusCode::OK, Json(serde_json::json!({ "bytes_reclaimed": reclaimed }))).into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("failed to prune images: {e}") }),
            )
                .into_response(),
        }
    } else {
        let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(node)) => node,
            Ok(None) => {
                return (StatusCode::NOT_FOUND, Json(ErrorResponse { error: format!("node '{}' not found", id) }))
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("database error: {e}") }),
                )
                    .into_response();
            }
        };

        let client = match crate::nodes::client::get_node_client(&state.node_clients, &id) {
            Ok(client) => client,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse { error: format!("node client not available: {e}") }),
                )
                    .into_response();
            }
        };

        let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
        match client.post(&format!("{}/images/prune", base_url)).send().await {
            Ok(resp) => {
                let body = resp.text().await.unwrap_or_default();
                (StatusCode::OK, body).into_response()
            }
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse { error: format!("failed to prune images on agent: {e}") }),
            )
                .into_response(),
        }
    }
}
