mod connect;
mod images;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::Arc;

use crate::AppState;
use crate::nodes::client::build_node_client;
use litebin_common::types::{Node, NodeStatus};

pub use connect::*;
pub use images::*;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateNodeRequest {
    pub name: String,
    pub host: String,
    pub agent_port: Option<i64>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ConflictResponse {
    pub error: String,
    pub project_ids: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NodeResponse {
    #[serde(flatten)]
    pub node: Node,
    pub recommended: bool,
}

fn recommended_node_id(nodes: &[Node]) -> Option<String> {
    let online: Vec<&Node> = nodes.iter().filter(|n| n.status == NodeStatus::Online).collect();
    if online.is_empty() {
        return None;
    }

    const MIN_DISK_FREE: i64 = 2 * 1024 * 1024 * 1024; // 2 GB

    let best = online.iter().min_by_key(|n| {
        let total = n.total_memory.unwrap_or(0).max(1);
        let available = n.available_memory.unwrap_or(0);
        let mem_used_pct = ((total.saturating_sub(available)) * 100) / total;
        let load_score = mem_used_pct + (n.container_count * 10);
        let disk_penalty = if n.disk_free.unwrap_or(0) < MIN_DISK_FREE { 1000 } else { 0 };
        load_score + disk_penalty
    });

    best.map(|n| n.id.clone())
}

#[utoipa::path(
    get,
    path = "/nodes",
    responses(
        (status = 200, description = "List of nodes", body = Vec<NodeResponse>),
        (status = 500, description = "Internal server error"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
pub async fn list_nodes(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_as::<_, Node>("SELECT * FROM nodes ORDER BY created_at ASC").fetch_all(&state.db).await {
        Ok(nodes) => {
            let rec_id = recommended_node_id(&nodes);
            let response: Vec<NodeResponse> = nodes
                .into_iter()
                .map(|node| NodeResponse { recommended: rec_id.as_deref() == Some(&node.id), node })
                .collect();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("database error: {e}") }))
            .into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/nodes",
    request_body = CreateNodeRequest,
    responses(
        (status = 201, description = "Node created"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
pub async fn create_node(State(state): State<AppState>, Json(req): Json<CreateNodeRequest>) -> impl IntoResponse {
    let agent_port = req.agent_port.unwrap_or(5083);

    // Generate a new node ID and shared secret
    let node_id = uuid::Uuid::new_v4().to_string();
    let agent_secret = format!("{:x}", sha2::Sha256::digest(uuid::Uuid::new_v4().to_string()));
    let now = chrono::Utc::now().timestamp();
    let region = req.region.clone();

    // Insert node with pending_setup status — no health check yet
    let result = sqlx::query(
        r#"
        INSERT INTO nodes (id, name, host, public_ip, agent_port, region, status, fail_count, agent_secret, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, 'pending_setup', 0, ?, ?, ?)
        "#,
    )
    .bind(&node_id)
    .bind(&req.name)
    .bind(&req.host)
    .bind(&req.public_ip)
    .bind(agent_port)
    .bind(&region)
    .bind(&agent_secret)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("database error: {e}") }))
            .into_response();
    }

    // Build mTLS client and add to pool for future connect calls
    if let Ok(client) =
        build_node_client(&state.config.ca_cert_path, &state.config.client_cert_path, &state.config.client_key_path)
    {
        state.node_clients.insert(node_id.clone(), Arc::new(client));
    }

    // Fetch the created node
    let node =
        match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?").bind(&node_id).fetch_one(&state.db).await {
            Ok(node) => node,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: format!("failed to fetch created node: {e}") }),
                )
                    .into_response();
            }
        };

    // Return the secret in the response (shown only once at creation)
    let mut response = serde_json::to_value(&node).unwrap_or_default();
    response["agent_secret"] = serde_json::Value::String(agent_secret);

    (StatusCode::CREATED, Json(response)).into_response()
}

#[utoipa::path(
    delete,
    path = "/nodes/{id}",
    params(
        ("id" = String, Path, description = "Node ID"),
    ),
    responses(
        (status = 204, description = "Node deleted"),
        (status = 400, description = "Bad request"),
        (status = 409, description = "Conflict"),
        (status = 404, description = "Node not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
pub async fn delete_node(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    // Reject decommissioning the local node
    if id == "local" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "cannot decommission the local node".to_string() }),
        )
            .into_response();
    }

    // Reject if any project still references this node (projects.node_id has no
    // ON DELETE CASCADE, so deleting would trip the FK). Caller must clear them first.
    let projects_on_node = sqlx::query_as::<_, (String,)>("SELECT id FROM projects WHERE node_id = ?")
        .bind(&id)
        .fetch_all(&state.db)
        .await;

    match projects_on_node {
        Ok(projects) if !projects.is_empty() => {
            let project_ids: Vec<String> = projects.into_iter().map(|(pid,)| pid).collect();
            return (
                StatusCode::CONFLICT,
                Json(ConflictResponse {
                    error: "delete all projects on this node before removing the node".to_string(),
                    project_ids,
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("database error: {e}") }))
                .into_response();
        }
        _ => {}
    }

    // Delete node from DB
    if let Err(e) = sqlx::query("DELETE FROM nodes WHERE id = ?").bind(&id).execute(&state.db).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("database error: {e}") }))
            .into_response();
    }

    // Remove from client pool
    state.node_clients.remove(&id);

    StatusCode::NO_CONTENT.into_response()
}
