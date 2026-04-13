use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use litebin_common::routing::RoutingProvider;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use litebin_common::caddy::CaddyClient;
use litebin_common::cloudflare::CloudflareClient;
use litebin_common::routing::ProjectRoute;

use crate::config::Config;
use crate::nodes::client::get_node_client;

/// Routes traffic via Cloudflare DNS records pointing to each node's public IP.
/// Each node runs its own Caddy for TLS termination and reverse proxy.
pub struct CloudflareDnsRouter {
    cloudflare: CloudflareClient,
    master_caddy: CaddyClient,
    node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
    db: SqlitePool,
    config: Arc<Config>,
}

impl CloudflareDnsRouter {
    pub fn new(
        cloudflare: CloudflareClient,
        master_caddy: CaddyClient,
        node_clients: Arc<DashMap<String, Arc<reqwest::Client>>>,
        db: SqlitePool,
        config: Arc<Config>,
    ) -> Self {
        Self {
            cloudflare,
            master_caddy,
            node_clients,
            db,
            config,
        }
    }

    /// Build Caddy config for the master node (local projects + dashboard/API).
    fn build_master_caddy_config(
        local_projects: &[&ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> Value {
        let logging = litebin_common::heartbeat::caddy_logging_config();
        let mut routes: Vec<Value> = Vec::new();
        for p in local_projects {
            routes.push(json!({
                "match": [{ "host": [p.subdomain_host] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": p.upstream }],
                    "handle_response": [{
                        "match": { "status_code": [502, 503, 504] },
                        "routes": [{
                            "handle": [{
                                "handler": "reverse_proxy",
                                "upstreams": [{ "dial": orchestrator_upstream }]
                            }]
                        }]
                    }]
                }]
            }));

            if let Some(cd) = &p.custom_domain {
                if let Some(ref rewrite) = p.host_rewrite {
                    // Sleeping custom domain: proxy to orchestrator waker with Host rewrite
                    let (www_host, _canonical) = if cd.starts_with("www.") {
                        (cd[4..].to_string(), cd.clone())
                    } else {
                        (format!("www.{}", cd), cd.clone())
                    };

                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": orchestrator_upstream }],
                            "headers": {
                                "request": {
                                    "set": { "Host": [rewrite] }
                                }
                            }
                        }]
                    }));

                    // Www variant also wakes (no redirect while sleeping)
                    routes.push(json!({
                        "match": [{ "host": [www_host] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": orchestrator_upstream }],
                            "headers": {
                                "request": {
                                    "set": { "Host": [rewrite] }
                                }
                            }
                        }]
                    }));
                } else {
                    // Running custom domain: proxy to container with 502 fallback
                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": p.upstream }],
                            "handle_response": [{
                                "match": { "status_code": [502, 503, 504] },
                                "routes": [{
                                    "handle": [{
                                        "handler": "reverse_proxy",
                                        "upstreams": [{ "dial": orchestrator_upstream }]
                                    }]
                                }]
                            }]
                        }]
                    }));

                    let (redirect_from, canonical) = if cd.starts_with("www.") {
                        (cd[4..].to_string(), cd.clone())
                    } else {
                        (format!("www.{}", cd), cd.clone())
                    };
                    routes.push(json!({
                        "match": [{ "host": [redirect_from] }],
                        "handle": [{
                            "handler": "static_response",
                            "status_code": 301,
                            "headers": { "Location": [format!("https://{}{{{{uri}}}}", canonical)] }
                        }]
                    }));
                }
            }
        }

        // Caddy ask endpoint
        routes.push(json!({
            "match": [{ "path": ["/caddy/ask"] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Dashboard + API routes
        let dashboard_host = format!("{}.{}", dashboard_subdomain, domain);
        routes.push(json!({
            "match": [{ "host": [dashboard_host] }],
            "handle": [{
                "handler": "subroute",
                "routes": [
                    {
                        "match": [{ "path": ["/auth/*", "/projects", "/projects/*", "/deploy", "/deploy-tokens", "/deploy-tokens/*", "/images", "/images/*", "/health", "/nodes", "/nodes/*", "/settings", "/settings/*", "/system/*", "/caddy/*"] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": orchestrator_upstream }]
                        }]
                    },
                    {
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": "dashboard:80" }]
                        }]
                    }
                ]
            }]
        }));

        // Poke subdomain: only /internal/* routes (wake-report endpoint)
        let poke_host = format!("{}.{}", poke_subdomain, domain);
        routes.push(json!({
            "match": [{ "host": [poke_host], "path": ["/internal/*"] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Catch-all for sleeping local apps → waker
        routes.push(json!({
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        let error_routes = json!({
            "routes": [{
                "match": [{ "host": [format!("*.{}", domain)] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": orchestrator_upstream }]
                }]
            }]
        });

        json!({
            "admin": { "listen": "0.0.0.0:2019" },
            "logging": logging["logging"],
            "apps": {
                "http": {
                    "servers": {
                        "srv0": {
                            "listen": [":80", ":443"],
                            "routes": routes,
                            "errors": error_routes,
                            "logs": {}
                        }
                    }
                },
                "tls": {
                    "automation": {
                        "on_demand": {
                            "permission": {
                                "endpoint": format!("http://{}/caddy/ask", orchestrator_upstream),
                                "module": "http"
                            }
                        },
                        "policies": [
                            { "subjects": ["localhost", "*.localhost"], "issuers": [{ "module": "internal" }] },
                            { "on_demand": true }
                        ]
                    }
                }
            }
        })
    }

    /// Build Caddy config for an agent node (only its projects + wake catch-all).
    fn build_agent_caddy_config(
        agent_projects: &[&ProjectRoute],
        domain: &str,
        orchestrator_url: &str,
    ) -> Value {
        let mut routes: Vec<Value> = Vec::new();

        for p in agent_projects {
            // Only create subdomain route for running projects.
            // Sleeping projects rely on the catch-all *.{domain} → agent wake handler.
            if p.host_rewrite.is_none() {
                let dial = p.container_upstream.as_deref().unwrap_or(&p.upstream);
                routes.push(json!({
                    "match": [{ "host": [p.subdomain_host] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": dial }]
                    }]
                }));
            }

            if let Some(cd) = &p.custom_domain {
                if let Some(ref rewrite) = p.host_rewrite {
                    // Sleeping custom domain: proxy to agent internal wake server with Host rewrite
                    let agent_wake = "litebin-agent:8444".to_string();
                    let (www_host, _canonical) = if cd.starts_with("www.") {
                        (cd[4..].to_string(), cd.clone())
                    } else {
                        (format!("www.{}", cd), cd.clone())
                    };

                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": agent_wake }],
                            "headers": {
                                "request": {
                                    "set": { "Host": [rewrite] }
                                }
                            }
                        }]
                    }));

                    // Www variant also wakes (no redirect while sleeping)
                    routes.push(json!({
                        "match": [{ "host": [www_host] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": agent_wake }],
                            "headers": {
                                "request": {
                                    "set": { "Host": [rewrite] }
                                }
                            }
                        }]
                    }));
                } else {
                    // Running custom domain: proxy to container
                    let dial = p.container_upstream.as_deref().unwrap_or(&p.upstream);
                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": dial }]
                        }]
                    }));

                    let (redirect_from, canonical) = if cd.starts_with("www.") {
                        (cd[4..].to_string(), cd.clone())
                    } else {
                        (format!("www.{}", cd), cd.clone())
                    };
                    routes.push(json!({
                        "match": [{ "host": [redirect_from] }],
                        "handle": [{
                            "handler": "static_response",
                            "status_code": 301,
                            "headers": { "Location": [format!("https://{}{{{{uri}}}}", canonical)] }
                        }]
                    }));
                }
            }
        }

        // Catch-all for sleeping apps on this agent → agent internal wake server
        routes.push(json!({
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": "litebin-agent:8444" }]
            }]
        }));

        let error_routes = json!({
            "routes": [{
                "match": [{ "host": [format!("*.{}", domain)] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": "litebin-agent:8444" }]
                }]
            }]
        });

        let ask_endpoint = if orchestrator_url.is_empty() {
            // Fallback: use agent's own permission endpoint (Docker network)
            "http://litebin-agent:8444/internal/caddy-ask".to_string()
        } else {
            format!("http://{}/caddy/ask", orchestrator_url)
        };

        let logging = litebin_common::heartbeat::caddy_logging_config();

        json!({
            "admin": { "listen": "0.0.0.0:2019" },
            "logging": logging["logging"],
            "apps": {
                "http": {
                    "servers": {
                        "srv0": {
                            "listen": [":80", ":443"],
                            "routes": routes,
                            "errors": error_routes,
                            "logs": {}
                        }
                    }
                },
                "tls": {
                    "automation": {
                        "on_demand": {
                            "permission": {
                                "endpoint": ask_endpoint,
                                "module": "http"
                            }
                        },
                        "policies": [{ "on_demand": true }]
                    }
                }
            }
        })
    }

    /// Push Caddy config to a remote agent via POST /caddy/sync.
    async fn push_agent_caddy(
        &self,
        node_id: &str,
        config: &Value,
    ) -> anyhow::Result<()> {
        let client = get_node_client(&self.node_clients, node_id)?;

        // Look up agent connection info
        let node: Option<(String, i64)> = sqlx::query_as(
            "SELECT host, agent_port FROM nodes WHERE id = ?",
        )
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
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(config)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("agent /caddy/sync failed ({}): {}", status, body);
        }

        Ok(())
    }

    /// Sync Cloudflare DNS records: upsert for running projects, delete stale ones.
    /// Returns counts of created, deleted, and errored records.
    async fn sync_dns(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> anyhow::Result<litebin_common::routing::DnsSyncResult> {
        // Compute desired DNS records
        let mut desired: HashMap<String, String> = HashMap::new(); // name → ip

        // Dashboard subdomain A record → master node public IP
        if !self.config.public_ip.is_empty() {
            let dashboard_host = format!("{}.{}", dashboard_subdomain, domain);
            desired.insert(dashboard_host, self.config.public_ip.clone());
        }

        // Poke subdomain A record → master node public IP
        if !self.config.public_ip.is_empty() {
            let poke_host = format!("{}.{}", poke_subdomain, domain);
            desired.insert(poke_host, self.config.public_ip.clone());
        }

        for p in projects {
            let ip = match &p.node_public_ip {
                Some(ip) if !ip.is_empty() => ip.clone(),
                _ => {
                    tracing::warn!(
                        project_id = %p.project_id,
                        "skipping DNS record — node has no public_ip"
                    );
                    continue;
                }
            };

            // Subdomain A record
            desired.insert(p.subdomain_host.clone(), ip.clone());

            // Custom domain A record
            if let Some(cd) = &p.custom_domain {
                desired.insert(cd.clone(), ip.clone());

                // Also add the www variant as a redirect handled by Caddy,
                // but we still need a DNS record pointing to the same IP
                let www = if cd.starts_with("www.") {
                    cd[4..].to_string()
                } else {
                    format!("www.{}", cd)
                };
                desired.insert(www, ip.clone());
            }
        }

        // Also keep DNS records for sleeping projects that have custom domains,
        // so the domain still resolves and reaches the waker.
        let sleeping_cd: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT p.id, p.node_id, p.custom_domain FROM projects p \
             WHERE p.status IN ('stopped', 'stopping') \
             AND p.custom_domain IS NOT NULL AND p.custom_domain != ''",
        )
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();

        for (_project_id, node_id, custom_domain) in &sleeping_cd {
            let node_ip = match node_id.as_deref() {
                Some(nid) if nid != "local" => {
                    let row: Option<(Option<String>,)> = sqlx::query_as(
                        "SELECT public_ip FROM nodes WHERE id = ?",
                    )
                    .bind(nid)
                    .fetch_optional(&self.db)
                    .await
                    .unwrap_or(None);
                    row.and_then(|(ip,)| ip)
                }
                _ => {
                    if self.config.public_ip.is_empty() {
                        continue;
                    }
                    Some(self.config.public_ip.clone())
                }
            };

            if let Some(ip) = node_ip {
                if !ip.is_empty() {
                    desired.insert(custom_domain.clone(), ip.clone());
                    let www = if custom_domain.starts_with("www.") {
                        custom_domain[4..].to_string()
                    } else {
                        format!("www.{}", custom_domain)
                    };
                    desired.insert(www, ip);
                }
            }
        }

        let domain_suffix = format!(".{}", domain);

        // List existing A records for our domain
        let existing = self
            .cloudflare
            .list_records_by_suffix(&domain_suffix, "A")
            .await?;

        let mut result = litebin_common::routing::DnsSyncResult::default();

        // Build set of existing record names for fast lookup
        let existing_names: HashSet<&str> = existing.iter().map(|r| r.name.as_str()).collect();

        // Delete records that exist but shouldn't
        let desired_names: HashSet<&str> = desired.keys().map(|s| s.as_str()).collect();
        for record in &existing {
            if !desired_names.contains(record.name.as_str()) {
                if let Err(e) = self.cloudflare.delete_record(&record.id).await {
                    tracing::warn!(record = %record.name, error = %e, "failed to delete stale DNS record");
                    result.errors += 1;
                } else {
                    result.deleted += 1;
                }
            }
        }

        // Upsert desired records
        for (name, ip) in &desired {
            if existing_names.contains(name.as_str()) {
                result.unchanged += 1;
                continue;
            }
            match self.cloudflare.upsert_record(name, "A", ip, 1, false).await {
                Ok(true) => result.created += 1,
                Ok(false) => result.unchanged += 1,
                Err(e) => {
                    tracing::warn!(name, ip, error = %e, "failed to upsert DNS record");
                    result.errors += 1;
                }
            }
        }

        tracing::info!(
            created = result.created,
            unchanged = result.unchanged,
            deleted = result.deleted,
            errors = result.errors,
            desired = desired.len(),
            existing = existing.len(),
            "DNS sync complete"
        );

        Ok(result)
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
        tracing::info!(
            route_count = projects.len(),
            "syncing routes (cloudflare_dns mode)"
        );

        // Group projects by node
        let mut by_node: HashMap<String, Vec<&ProjectRoute>> = HashMap::new();
        for p in projects {
            let node_key = p.node_id.clone().unwrap_or_else(|| "local".to_string());
            by_node.entry(node_key).or_default().push(p);
        }

        // 1. Master Caddy — local projects + dashboard/API
        let local_projects = by_node.get("local").cloned().unwrap_or_default();
        let master_config =
            Self::build_master_caddy_config(&local_projects, domain, orchestrator_upstream, dashboard_subdomain, poke_subdomain);

        let url = format!("{}/load", self.master_caddy.admin_url());
        let resp = self
            .master_caddy
            .post_json(&url, &master_config)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, "master caddy /load failed: {}", body);
        } else {
            tracing::info!(local_count = local_projects.len(), "master caddy config loaded");
        }

        // 2. Agent Caddys — push config for remote projects
        for (node_id, agent_projects) in &by_node {
            if node_id == "local" {
                continue;
            }

            let agent_config = Self::build_agent_caddy_config(
                agent_projects,
                domain,
                &self.config.public_ip, // orchestrator reachable for /caddy/ask
            );

            if let Err(e) = self.push_agent_caddy(node_id, &agent_config).await {
                tracing::warn!(node_id, error = %e, "failed to push caddy config to agent");
            }

            // Push project metadata (auto_start_enabled flags) to agent
            push_project_meta_to_agent(
                node_id,
                &self.db,
                &self.node_clients,
                &self.config,
            )
            .await;
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

/// Push project metadata (auto_start_enabled flags) to a remote agent.
/// Called during route sync and on settings toggle.
pub async fn push_project_meta_to_agent(
    node_id: &str,
    db: &SqlitePool,
    node_clients: &DashMap<String, Arc<reqwest::Client>>,
    config: &Config,
) {
    // Query all projects for this node
    let rows: Vec<(String, bool)> = match sqlx::query_as(
        "SELECT id, auto_start_enabled FROM projects WHERE node_id = ?",
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

    let projects: HashMap<String, bool> = rows.into_iter().collect();

    let client = match get_node_client(node_clients, node_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(node_id, error = %e, "skipping project meta push: no client");
            return;
        }
    };

    let node: Option<(String, i64)> = match sqlx::query_as(
        "SELECT host, agent_port FROM nodes WHERE id = ?",
    )
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
    let body = json!({ "projects": projects });

    match client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
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
