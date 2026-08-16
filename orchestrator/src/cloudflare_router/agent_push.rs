use std::collections::HashMap;

use litebin_common::types::Node;

use crate::nodes::client::{AgentClient, get_node_client};

use super::CloudflareDnsRouter;

impl CloudflareDnsRouter {
    /// Push Caddy config to a remote agent via POST /caddy/sync.
    pub(super) async fn push_agent_caddy(&self, node_id: &str, config: &serde_json::Value) -> anyhow::Result<()> {
        let client = get_node_client(&self.node_clients, node_id)?;

        // Look up agent connection info
        let node: Option<Node> =
            sqlx::query_as("SELECT * FROM nodes WHERE id = ?").bind(node_id).fetch_optional(&self.db).await?;

        let node = match node {
            Some(n) => n,
            None => {
                tracing::warn!(node_id, "node not found in DB, skipping agent caddy push");
                return Ok(());
            }
        };

        let agent = AgentClient::new(client, &node, &self.config);
        agent.caddy_sync(config).await.map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Push project metadata (auto_start_enabled flags) to a remote agent.
/// Called during route sync and on settings toggle.
pub async fn push_project_meta_to_agent(
    node_id: &str,
    db: &sqlx::SqlitePool,
    node_clients: &dashmap::DashMap<String, std::sync::Arc<reqwest::Client>>,
    config: &crate::config::Config,
) {
    // Query all projects for this node
    let rows: Vec<(String, bool, bool, bool)> = match sqlx::query_as(
        "SELECT id, auto_start_enabled, allow_raw_ports, is_background FROM projects WHERE node_id = ?",
    )
    .bind(node_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(node_id, error = %e, "failed to query projects for meta push");
            return;
        }
    };

    let projects: HashMap<String, bool> = rows.iter().map(|(id, auto, _, _)| (id.clone(), *auto)).collect();
    let background_projects: HashMap<String, bool> =
        rows.iter().filter(|(_, _, _, background)| *background).map(|(id, _, _, _)| (id.clone(), true)).collect();
    let allow_raw_ports: HashMap<String, bool> =
        rows.iter().filter(|(_, _, raw, _)| *raw).map(|(id, _, _, _)| (id.clone(), true)).collect();
    let docker_observe: HashMap<String, bool> = sqlx::query_scalar::<_, String>(
        "SELECT pc.project_id FROM project_capabilities pc \
         JOIN projects p ON p.id = pc.project_id \
         WHERE p.node_id = ? AND pc.capability = 'docker-observe'",
    )
    .bind(node_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|id| (id, true))
    .collect();
    let host_network: HashMap<String, bool> = sqlx::query_scalar::<_, String>(
        "SELECT pc.project_id FROM project_capabilities pc \
         JOIN projects p ON p.id = pc.project_id \
         WHERE p.node_id = ? AND p.is_background = 1 AND pc.capability = 'host-network'",
    )
    .bind(node_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|id| (id, true))
    .collect();

    let client = match get_node_client(node_clients, node_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(node_id, error = %e, "skipping project meta push: no client");
            return;
        }
    };

    let node: Option<Node> =
        match sqlx::query_as("SELECT * FROM nodes WHERE id = ?").bind(node_id).fetch_optional(db).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(node_id, error = %e, "failed to look up node for meta push");
                return;
            }
        };

    let Some(node) = node else { return };

    let agent = AgentClient::new(client, &node, config);

    // Read global defaults to push to agent
    let default_mem: i64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_memory_limit_mb'")
        .fetch_one(db)
        .await
        .ok()
        .and_then(|v: String| v.parse().ok())
        .unwrap_or(256);
    let default_cpu: f64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_cpu_limit'")
        .fetch_one(db)
        .await
        .ok()
        .and_then(|v: String| v.parse().ok())
        .unwrap_or(0.5);

    let body = litebin_common::agent_api::ProjectMetaRequest {
        projects,
        background_projects: Some(background_projects),
        allow_raw_ports: Some(allow_raw_ports),
        docker_observe: Some(docker_observe),
        host_network: Some(host_network),
        default_memory_limit_mb: Some(default_mem),
        default_cpu_limit: Some(default_cpu),
    };

    match agent.push_project_meta(&body).await {
        Ok(()) => {
            tracing::info!(node_id, count = body.projects.len(), "pushed project meta to agent");
        }
        Err(crate::nodes::client::AgentClientError::Status { code, .. }) => {
            tracing::warn!(
                node_id,
                status = %code,
                "failed to push project meta to agent"
            );
        }
        Err(e) => {
            tracing::debug!(
                node_id,
                error = %e,
                "project meta push failed (agent may be down)"
            );
        }
    }
}
