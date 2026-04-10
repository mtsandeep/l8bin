use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::Arc;

use crate::AppState;
use crate::nodes::client::build_node_client;
use litebin_common::types::{HealthReport, ImageStats, Node};

#[derive(Deserialize)]
pub struct CreateNodeRequest {
    pub name: String,
    pub host: String,
    pub agent_port: Option<i64>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Serialize)]
pub struct ConflictResponse {
    pub error: String,
    pub project_ids: Vec<String>,
}

pub async fn list_nodes(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_as::<_, Node>("SELECT * FROM nodes ORDER BY created_at ASC")
        .fetch_all(&state.db)
        .await
    {
        Ok(nodes) => (StatusCode::OK, Json(nodes)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("database error: {e}"),
            }),
        )
            .into_response(),
    }
}

pub async fn create_node(
    State(state): State<AppState>,
    Json(req): Json<CreateNodeRequest>,
) -> impl IntoResponse {
    let agent_port = req.agent_port.unwrap_or(8443);

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
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("database error: {e}"),
            }),
        )
            .into_response();
    }

    // Build mTLS client and add to pool for future connect calls
    if let Ok(client) = build_node_client(
        &state.config.ca_cert_path,
        &state.config.client_cert_path,
        &state.config.client_key_path,
    ) {
        state
            .node_clients
            .insert(node_id.clone(), Arc::new(client));
    }

    // Fetch the created node
    let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&node_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(node) => node,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("failed to fetch created node: {e}"),
                }),
            )
                .into_response();
        }
    };

    // Return the secret in the response (shown only once at creation)
    let mut response = serde_json::to_value(&node).unwrap_or_default();
    response["agent_secret"] = serde_json::Value::String(agent_secret);

    (StatusCode::CREATED, Json(response)).into_response()
}

/// POST /nodes/{id}/connect — health check + push config to agent via mTLS.
/// Transitions node from pending_setup → online.
pub async fn connect_node(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // 1. Look up node from DB
    let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "node not found".to_string(),
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("database error: {e}"),
                }),
            )
                .into_response();
        }
    };

    // Only connect pending_setup or offline nodes
    if node.status != "pending_setup" && node.status != "offline" {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!(
                    "node status is '{}', expected 'pending_setup' or 'offline'",
                    node.status
                ),
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
                        Json(ErrorResponse {
                            error: format!("cannot build mTLS client: {e}"),
                        }),
                    )
                        .into_response();
                }
            }
        }
    };

    // 3. Health check via mTLS
    let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
    let health: HealthReport = match client
        .get(&format!("{}/health", base_url))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<HealthReport>().await {
            Ok(h) => h,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(ErrorResponse {
                        error: format!("failed to parse health response: {e}"),
                    }),
                )
                    .into_response();
            }
        },
        Ok(resp) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!("agent returned non-success: {}", resp.status()),
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!("failed to connect to agent: {e}"),
                }),
            )
                .into_response();
        }
    };

    // 4. Push config to agent via POST /internal/register
    let secret = node.agent_secret.clone().unwrap_or_default();
    let register_body = serde_json::json!({
        "node_id": node.id,
        "secret": secret,
        "domain": state.config.domain,
        "wake_report_url": format_wake_report_url(&state),
        "heartbeat_url": format_heartbeat_url(&state),
    });

    match client
        .post(&format!("{}/internal/register", base_url))
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
                Json(ErrorResponse {
                    error: format!("agent rejected registration: {body}"),
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: format!("failed to push config to agent: {e}"),
                }),
            )
                .into_response();
        }
    }

    // 5. Update node status to online
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE nodes SET status = 'online', fail_count = 0, total_memory = ?, total_cpu = ?, public_ip = ?, last_seen_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(health.memory_total as i64)
    .bind(health.cpu_cores as f64)
    .bind(&health.public_ip)
    .bind(now)
    .bind(now)
    .bind(&id)
    .execute(&state.db)
    .await;

    let updated_node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await;

    match updated_node {
        Ok(n) => (StatusCode::OK, Json(n)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("failed to fetch updated node: {e}"),
            }),
        )
            .into_response(),
    }
}

/// Build the wake_report_url for agents to POST to.
pub fn format_wake_report_url(state: &AppState) -> String {
    if state.config.ca_cert_path.is_empty() {
        // Dev mode: direct to orchestrator over HTTP
        format!(
            "http://localhost:{}/internal/wake-report",
            state.config.port
        )
    } else {
        // Production: route through Caddy (port 443) for proper TLS
        format!(
            "https://{}.{}/internal/wake-report",
            state.config.poke_subdomain,
            state.config.domain
        )
    }
}

/// Build the heartbeat_url for agents to POST activity data to.
pub fn format_heartbeat_url(state: &AppState) -> String {
    if state.config.ca_cert_path.is_empty() {
        format!(
            "http://localhost:{}/internal/heartbeat",
            state.config.port
        )
    } else {
        // Production: route through Caddy (port 443) for proper TLS
        format!(
            "https://{}.{}/internal/heartbeat",
            state.config.poke_subdomain,
            state.config.domain
        )
    }
}

pub async fn delete_node(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Reject decommissioning the local node
    if id == "local" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "cannot decommission the local node".to_string(),
            }),
        )
            .into_response();
    }

    // Check for running projects on this node
    let running_projects = sqlx::query_as::<_, (String,)>(
        "SELECT id FROM projects WHERE status = 'running' AND node_id = ?",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await;

    match running_projects {
        Ok(projects) if !projects.is_empty() => {
            let project_ids: Vec<String> = projects.into_iter().map(|(pid,)| pid).collect();
            return (
                StatusCode::CONFLICT,
                Json(ConflictResponse {
                    error: "node has running projects".to_string(),
                    project_ids,
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("database error: {e}"),
                }),
            )
                .into_response();
        }
        _ => {}
    }

    // Delete node from DB
    if let Err(e) = sqlx::query("DELETE FROM nodes WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("database error: {e}"),
            }),
        )
            .into_response();
    }

    // Remove from client pool
    state.node_clients.remove(&id);

    StatusCode::NO_CONTENT.into_response()
}

// ── Image Stats ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct NodeImageStatsResponse {
    pub node_id: String,
    pub node_name: String,
    pub image_stats: ImageStats,
}

/// GET /nodes/image-stats — returns image statistics for each node.
pub async fn node_image_stats(State(state): State<AppState>) -> impl IntoResponse {
    let mut results = Vec::new();

    // Local node
    let stats = state.docker.image_stats().await;
    let name = sqlx::query_scalar::<_, String>(
        "SELECT name FROM nodes WHERE id = 'local'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "local".to_string());

    results.push(NodeImageStatsResponse {
        node_id: "local".to_string(),
        node_name: name,
        image_stats: stats,
    });

    // Remote nodes: get stats from agent health endpoint
    let nodes = match sqlx::query_as::<_, Node>(
        "SELECT * FROM nodes WHERE id != 'local' ORDER BY name",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(nodes) => nodes,
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch remote nodes for image stats");
            return (StatusCode::OK, Json(results)).into_response();
        }
    };

    for node in nodes {
        let client = match crate::nodes::client::get_node_client(&state.node_clients, &node.id) {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!(node_id = %node.id, error = %e, "no client for remote node");
                continue;
            }
        };
        let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
        match client.get(&format!("{}/health", base_url)).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(health) = resp.json::<HealthReport>().await {
                    results.push(NodeImageStatsResponse {
                        node_id: node.id.clone(),
                        node_name: node.name.clone(),
                        image_stats: health.image_stats,
                    });
                }
            }
            Ok(resp) => {
                tracing::warn!(node_id = %node.id, status = %resp.status(), "agent returned non-success for image stats");
            }
            Err(e) => {
                tracing::warn!(node_id = %node.id, error = %e, "failed to get image stats from remote node");
            }
        }
    }

    (StatusCode::OK, Json(results)).into_response()
}

/// POST /nodes/{id}/images/prune — prune dangling images on a specific node.
pub async fn prune_node_images(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if id == "local" {
        match state.docker.prune_dangling_images().await {
            Ok(reclaimed) => (
                StatusCode::OK,
                Json(serde_json::json!({ "bytes_reclaimed": reclaimed })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("failed to prune images: {e}"),
                }),
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
                return (
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("node '{}' not found", id),
                    }),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: format!("database error: {e}"),
                    }),
                )
                    .into_response();
            }
        };

        let client = match crate::nodes::client::get_node_client(&state.node_clients, &id) {
            Ok(client) => client,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: format!("node client not available: {e}"),
                    }),
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
                Json(ErrorResponse {
                    error: format!("failed to prune images on agent: {e}"),
                }),
            )
                .into_response(),
        }
    }
}
