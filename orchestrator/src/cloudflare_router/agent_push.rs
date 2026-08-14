use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::json;
use sqlx::SqlitePool;

use crate::config::Config;
use crate::nodes::client::get_node_client;

use super::CloudflareDnsRouter;

impl CloudflareDnsRouter {
    /// Push Caddy config to a remote agent via POST /caddy/sync.
    pub(super) async fn push_agent_caddy(&self, node_id: &str, config: &serde_json::Value) -> anyhow::Result<()> {
        let client = get_node_client(&self.node_clients, node_id)?;

        // Look up agent connection info
        let node: Option<(String, i64)> = sqlx::query_as("SELECT host, agent_port FROM nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(&self.db)
            .await?;

        let (host, agent_port) = match node {
            Some(h) => h,
            None => {
                tracing::warn!(node_id, "node not found in DB, skipping agent caddy push");
                return Ok(());
            }
        };

        let base_url = if self.config.ca_cert_path.is_empty() {
            format!("http://{}:{}", host, agent_port)
        } else {
            format!("https://{}:{}", host, agent_port)
        };

        let url = format!("{}/caddy/sync", base_url);
        let resp = client.post(&url).header("Content-Type", "application/json").json(config).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("agent /caddy/sync failed ({}): {}", status, body);
        }

        Ok(())
    }
}

/// Push project metadata (auto_start_enabled flags) to a remote agent.
/// Called during route sync and on settings toggle.
pub async fn push_project_meta_to_agent(
    node_id: &str,
    db: &SqlitePool,
    node_clients: &DashMap<String, Arc<reqwest::Client>>,
    config: &Config,
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

    let node: Option<(String, i64)> = match sqlx::query_as("SELECT host, agent_port FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(db)
        .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(node_id, error = %e, "failed to look up node for meta push");
            return;
        }
    };

    let (host, agent_port) = match node {
        Some(h) => h,
        None => return,
    };

    let base_url = if config.ca_cert_path.is_empty() {
        format!("http://{}:{}", host, agent_port)
    } else {
        format!("https://{}:{}", host, agent_port)
    };

    let url = format!("{}/internal/project-meta", base_url);

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

    let body = json!({
        "projects": projects,
        "background_projects": background_projects,
        "allow_raw_ports": allow_raw_ports,
        "docker_observe": docker_observe,
        "host_network": host_network,
        "default_memory_limit_mb": default_mem,
        "default_cpu_limit": default_cpu,
    });

    match client.post(&url).header("Content-Type", "application/json").json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(node_id, count = projects.len(), "pushed project meta to agent");
        }
        Ok(resp) => {
            tracing::warn!(
                node_id,
                status = %resp.status(),
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
