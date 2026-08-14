mod agent_push;
mod caddy_config;
mod dns;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use litebin_common::routing::{ProjectRoute, RoutingProvider};
use sqlx::SqlitePool;

use litebin_common::caddy::CaddyClient;
use litebin_common::cloudflare::CloudflareClient;

use crate::config::Config;

pub use agent_push::push_project_meta_to_agent;

/// Routes traffic via Cloudflare DNS records pointing to each node's public IP.
/// Each node runs its own Caddy for TLS termination and reverse proxy.
pub struct CloudflareDnsRouter {
    pub(super) cloudflare: CloudflareClient,
    pub(super) master_caddy: CaddyClient,
    pub(super) node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
    pub(super) db: SqlitePool,
    pub(super) config: Arc<Config>,
}

impl CloudflareDnsRouter {
    pub fn new(
        cloudflare: CloudflareClient,
        master_caddy: CaddyClient,
        node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
        db: SqlitePool,
        config: Arc<Config>,
    ) -> Self {
        Self { cloudflare, master_caddy, node_clients, db, config }
    }
}

#[async_trait]
impl RoutingProvider for CloudflareDnsRouter {
    async fn sync_dns_only(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> anyhow::Result<litebin_common::routing::DnsSyncResult> {
        self.sync_dns(projects, domain, dashboard_subdomain, poke_subdomain).await
    }

    async fn sync_routes(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
        sync_dns: bool,
    ) -> anyhow::Result<()> {
        tracing::info!(route_count = projects.len(), "syncing routes (cloudflare_dns mode)");

        // Group projects by node
        let mut by_node: HashMap<String, Vec<&ProjectRoute>> = HashMap::new();
        for p in projects {
            let node_key = p.node_id.clone().unwrap_or_else(|| "local".to_string());
            by_node.entry(node_key).or_default().push(p);
        }

        // 1. Master Caddy — local projects + dashboard/API
        let local_projects = by_node.get("local").cloned().unwrap_or_default();
        let master_config = Self::build_master_caddy_config(
            &local_projects,
            domain,
            orchestrator_upstream,
            dashboard_subdomain,
            poke_subdomain,
        );

        let url = format!("{}/load", self.master_caddy.admin_url());
        let resp = self.master_caddy.post_json(&url, &master_config).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("master caddy /load failed ({status}): {body}");
        }
        tracing::info!(local_count = local_projects.len(), "master caddy config loaded");

        // 2. Agent Caddys — include nodes with no active routes so stale
        // project/custom-domain routes are removed when a project becomes background.
        let remote_node_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM nodes WHERE id != 'local' AND status != 'decommissioned'")
                .fetch_all(&self.db)
                .await?;
        for node_id in remote_node_ids {
            let agent_projects = by_node.get(&node_id).cloned().unwrap_or_default();

            let agent_config = Self::build_agent_caddy_config(
                &agent_projects,
                domain,
                &self.config.public_ip, // orchestrator reachable for /caddy/ask
            );

            if let Err(e) = self.push_agent_caddy(&node_id, &agent_config).await {
                tracing::warn!(node_id, error = %e, "failed to push caddy config to agent");
            }

            // Push project metadata (auto_start_enabled flags) to agent
            push_project_meta_to_agent(&node_id, &self.db, &self.node_clients, &self.config).await;
        }

        // 3. Cloudflare DNS sync (skip for periodic checks where nothing changed)
        if sync_dns {
            match self.sync_dns(projects, domain, dashboard_subdomain, poke_subdomain).await {
                Ok(r) => tracing::info!(created = r.created, deleted = r.deleted, errors = r.errors, "DNS sync done"),
                Err(e) => tracing::warn!(error = %e, "DNS sync failed"),
            }
        }

        Ok(())
    }
}
