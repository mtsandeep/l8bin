use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;

use crate::AppState;
use crate::nodes::client::build_node_client;
use litebin_common::types::{HealthReport, Node, NodeStatus};

use super::ErrorResponse;

#[utoipa::path(
    post,
    path = "/nodes/{id}/connect",
    params(
        ("id" = String, Path, description = "Node ID"),
    ),
    responses(
        (status = 200, description = "Node connected", body = Node),
        (status = 404, description = "Node not found"),
        (status = 409, description = "Node status conflict"),
        (status = 422, description = "Unprocessable entity"),
        (status = 503, description = "Service unavailable"),
    ),
    tag = "nodes",
    security(("session_auth" = [])),
)]
/// POST /nodes/{id}/connect — health check + push config to agent via mTLS.
/// Transitions node from pending_setup → online.
pub async fn connect_node(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    // 1. Look up node from DB
    let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(ErrorResponse { error: "node not found".to_string() }))
                .into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("database error: {e}") }))
                .into_response();
        }
    };

    // Only connect pending_setup or offline nodes
    if node.status != NodeStatus::PendingSetup && node.status != NodeStatus::Offline {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("node status is '{}', expected 'pending_setup' or 'offline'", node.status),
            }),
        )
            .into_response();
    }

    // 2. Get or build mTLS client
    let client = match crate::nodes::client::get_node_client(&state.node_clients, &id) {
        Ok(c) => c,
        Err(_) => {
            match build_node_client(
                &state.config.ca_cert_path,
                &state.config.client_cert_path,
                &state.config.client_key_path,
            ) {
                Ok(c) => {
                    state.node_clients.insert(id.clone(), Arc::new(c));
                    state.node_clients.get(&id).unwrap().value().clone()
                }
                Err(e) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(ErrorResponse { error: format!("cannot build mTLS client: {e}") }),
                    )
                        .into_response();
                }
            }
        }
    };

    // 3. Health check via mTLS
    let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
    let health: HealthReport = match client.get(&format!("{}/health", base_url)).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<HealthReport>().await {
            Ok(h) => h,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(ErrorResponse { error: format!("failed to parse health response: {e}") }),
                )
                    .into_response();
            }
        },
        Ok(resp) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: format!("agent returned non-success: {}", resp.status()) }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: format!("failed to connect to agent: {e}") }),
            )
                .into_response();
        }
    };

    // 4. Push config to agent via POST /internal/register
    let secret = node.agent_secret.clone().unwrap_or_default();
    let register_body = serde_json::json!({
        "node_id": node.id,
        "secret": secret,
        "domain": state.platform.domain(),
        "wake_report_url": format_wake_report_url(&state),
        "heartbeat_url": format_heartbeat_url(&state),
    });

    match client
        .post(&format!("{}{}", base_url, litebin_common::types::AGENT_REGISTER_PATH))
        .json(&register_body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(node_id = %id, "config pushed to agent");
        }
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: format!("agent rejected registration: {body}") }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse { error: format!("failed to push config to agent: {e}") }),
            )
                .into_response();
        }
    }

    // 5. Update node status to online
    let now = chrono::Utc::now().timestamp();
    if let Err(e) = sqlx::query(
        "UPDATE nodes SET status = 'online', fail_count = 0, total_memory = ?, total_cpu = ?, public_ip = ?, architecture = ?, version = ?, last_seen_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(health.memory_total as i64)
    .bind(health.cpu_cores as f64)
    .bind(&health.public_ip)
    .bind(&health.architecture)
    .bind(&health.version)
    .bind(now)
    .bind(now)
    .bind(&id)
    .execute(&state.db)
    .await
    {
        tracing::warn!(node_id = %id, error = %e, "nodes: failed to update node status on manual connect");
    }

    let updated_node =
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?").bind(&id).fetch_one(&state.db).await;

    match updated_node {
        Ok(n) => (StatusCode::OK, Json(n)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("failed to fetch updated node: {e}") }),
        )
            .into_response(),
    }
}

/// Build the wake_report_url for agents to POST to.
pub fn format_wake_report_url(state: &AppState) -> String {
    if state.config.ca_cert_path.is_empty() {
        // Dev mode: direct to orchestrator over HTTP
        format!("http://localhost:{}{}", state.config.port, litebin_common::types::WAKE_REPORT_PATH)
    } else {
        // Production: route through Caddy (port 443) for proper TLS
        format!(
            "https://{}.{}{}",
            state.platform.poke_subdomain(),
            state.platform.domain(),
            litebin_common::types::WAKE_REPORT_PATH
        )
    }
}

/// Build the heartbeat_url for agents to POST activity data to.
pub fn format_heartbeat_url(state: &AppState) -> String {
    if state.config.ca_cert_path.is_empty() {
        format!("http://localhost:{}{}", state.config.port, litebin_common::types::HEARTBEAT_PATH)
    } else {
        // Production: route through Caddy (port 443) for proper TLS
        format!(
            "https://{}.{}{}",
            state.platform.poke_subdomain(),
            state.platform.domain(),
            litebin_common::types::HEARTBEAT_PATH
        )
    }
}

/// Push /internal/register to all online remote agents with current platform domain + URLs.
/// Returns (success_count, error messages).
pub async fn reregister_online_agents(state: &AppState) -> (usize, Vec<String>) {
    let nodes = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE status = 'online' AND id != 'local'")
        .fetch_all(&state.db)
        .await
    {
        Ok(n) => n,
        Err(e) => return (0, vec![format!("failed to list nodes: {e}")]),
    };

    let mut ok = 0usize;
    let mut errs = Vec::new();

    for node in nodes {
        let client = match crate::nodes::client::get_node_client(&state.node_clients, &node.id) {
            Ok(c) => c,
            Err(_) => {
                match build_node_client(
                    &state.config.ca_cert_path,
                    &state.config.client_cert_path,
                    &state.config.client_key_path,
                ) {
                    Ok(c) => {
                        let arc = Arc::new(c);
                        state.node_clients.insert(node.id.clone(), arc.clone());
                        arc
                    }
                    Err(e) => {
                        errs.push(format!("node {}: cannot build client: {e}", node.id));
                        continue;
                    }
                }
            }
        };

        let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
        let secret = node.agent_secret.clone().unwrap_or_default();
        let register_body = serde_json::json!({
            "node_id": node.id,
            "secret": secret,
            "domain": state.platform.domain(),
            "wake_report_url": format_wake_report_url(state),
            "heartbeat_url": format_heartbeat_url(state),
        });

        match client
            .post(&format!("{}{}", base_url, litebin_common::types::AGENT_REGISTER_PATH))
            .json(&register_body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                ok += 1;
                tracing::info!(node_id = %node.id, "re-registered agent with new platform domain");
            }
            Ok(resp) => {
                let body = resp.text().await.unwrap_or_default();
                errs.push(format!("node {}: agent rejected registration: {body}", node.id));
            }
            Err(e) => {
                errs.push(format!("node {}: failed to register: {e}", node.id));
            }
        }
    }

    (ok, errs)
}
