mod compose_transform;
mod import;

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use axum_login::AuthSession;
use litebin_common::{
    scan::{ScanGroup, ScanResult},
    types::Node,
};
use serde::{Deserialize, Serialize};

use crate::{AppState, auth::backend::PasswordBackend, nodes, routes::manage::get_node_from_db};
use import::import_single_group;

// ── Scan ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ScanQuery {
    pub node_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/scan",
    description = "Find foreign (non-LiteBin) container groups on a node: local, all, or a specific node ID.",
    params(ScanQuery),
    responses(
        (status = 200, body = litebin_common::scan::ScanResult),
        (status = 404, description = "Node not found"),
    ),
    tag = "scan",
    security(("session_auth" = []))
)]
pub async fn scan_containers(State(state): State<AppState>, Query(q): Query<ScanQuery>) -> impl IntoResponse {
    let node_id = q.node_id.as_deref().unwrap_or("all");

    // ── Local scan ──────────────────────────────────────────────────────────
    let local_groups = if node_id == "local" || node_id == "all" {
        match state.docker.scan_foreign_containers().await {
            Ok(groups) => groups,
            Err(e) => {
                tracing::error!(error = %e, "scan: local scan failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // ── Remote agent scans ──────────────────────────────────────────────────
    let agent_nodes: Vec<Node> = if node_id == "all" {
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id != 'local' AND status = 'online' ORDER BY name")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
    } else if node_id != "local" {
        // Specific agent node
        match get_node_from_db(&state.db, node_id).await {
            Ok(n) => vec![n],
            Err((_, msg)) => {
                return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": msg }))).into_response();
            }
        }
    } else {
        Vec::new()
    };

    // Fan-out to all agent nodes in parallel
    let mut agent_futures = Vec::new();
    for node in &agent_nodes {
        let node_id_owned = node.id.clone();
        let client = match nodes::client::get_node_client(&state.node_clients, &node.id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(node_id = %node.id, error = %e, "scan: no client for node");
                continue;
            }
        };
        let agent = nodes::client::AgentClient::new(client, node, &state.config);
        let fut = async move {
            match agent.scan().await {
                Ok(groups) => (node_id_owned, groups),
                Err(e) => {
                    tracing::warn!(error = %e, "scan: agent request failed");
                    (node_id_owned, Vec::new())
                }
            }
        };
        agent_futures.push(fut);
    }

    let agent_results: Vec<(String, Vec<ScanGroup>)> = futures_util::future::join_all(agent_futures).await;

    let mut nodes_map: HashMap<String, Vec<ScanGroup>> = HashMap::new();
    for (nid, groups) in agent_results {
        nodes_map.insert(nid, groups);
    }

    (StatusCode::OK, Json(ScanResult { local: local_groups, nodes: nodes_map })).into_response()
}

// ── Import ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ImportGroupRequest {
    pub node_id: String,
    pub project_id: String,
    #[allow(dead_code)]
    pub group_key: String,
    pub public_service: Option<String>,
    pub setup_routing: bool,
    /// Full container data echoed back from the scan response.
    pub containers: Vec<litebin_common::scan::ScanContainer>,
    pub deploy_type: litebin_common::types::DeployType,
    pub compose_working_dir: Option<String>,
    pub compose_file_found: bool,
    #[allow(dead_code)]
    pub env_file_found: bool,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ImportRequest {
    pub groups: Vec<ImportGroupRequest>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ImportedGroup {
    pub project_id: String,
    pub node_id: String,
    pub containers_imported: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ImportResponse {
    pub imported: Vec<ImportedGroup>,
    pub errors: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/scan/import",
    request_body = ImportRequest,
    description = "Adopt scanned foreign container groups as LiteBin projects: write DB rows, resolve/rebuild compose, rewrite bind mounts, import into Docker, and optionally set up routing.",
    responses(
        (status = 200, body = ImportResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "scan",
    security(("session_auth" = []))
)]
pub async fn import_containers(
    State(state): State<AppState>,
    auth_session: AuthSession<PasswordBackend>,
    Json(req): Json<ImportRequest>,
) -> impl IntoResponse {
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "not authenticated" })))
                .into_response();
        }
    };
    let mut imported = Vec::new();
    let mut global_errors = Vec::new();

    // Pre-validate: no duplicate project_ids in the batch
    let mut seen_ids = std::collections::HashSet::new();
    for group in &req.groups {
        if !seen_ids.insert(group.project_id.clone()) {
            global_errors.push(format!("duplicate project_id '{}' in import request", group.project_id));
        }
    }
    // If there were duplicates, return early — don't import any
    if !global_errors.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ImportResponse { imported, errors: global_errors })).into_response();
    }

    for group in req.groups {
        match import_single_group(&state, &user_id, group).await {
            Ok((result, setup_routing)) => {
                if setup_routing {
                    let _ = state.route_sync_tx.send(());
                }
                imported.push(result);
            }
            Err(e) => {
                tracing::error!(error = %e, "import: group failed");
                global_errors.push(e);
            }
        }
    }

    (StatusCode::OK, Json(ImportResponse { imported, errors: global_errors })).into_response()
}
