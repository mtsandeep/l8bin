use serde_json::{json, Value};

use crate::types::Project;

/// API path prefixes that should be proxied to the orchestrator (not the dashboard).
pub const ORCHESTRATOR_API_PATHS: &[&str] = &[
    "/auth/*",
    "/projects",
    "/projects/*",
    "/deploy",
    "/deploy/*",
    "/deploy-tokens",
    "/deploy-tokens/*",
    "/images",
    "/images/*",
    "/health",
    "/nodes",
    "/nodes/*",
    "/settings",
    "/settings/*",
    "/system/*",
    "/caddy/*",
];

pub struct CaddyClient {
    admin_url: String,
    client: reqwest::Client,
}

impl CaddyClient {
    pub fn new(admin_url: &str) -> Self {
        Self {
            admin_url: admin_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn admin_url(&self) -> &str {
        &self.admin_url
    }

    pub async fn post_json(&self, url: &str, body: &serde_json::Value) -> anyhow::Result<reqwest::Response> {
        let resp = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }

    /// Rebuild and load the full Caddy config from the given list of running projects.
    /// This is the single source of truth — every call replaces the entire Caddy config.
    pub async fn sync_routes(
        &self,
        projects: &[Project],
        domain: &str,
        orchestrator_upstream: &str,
    ) -> anyhow::Result<()> {
        let mut routes: Vec<Value> = Vec::new();

        for project in projects {
            if project.status != "running" {
                continue;
            }
            let Some(internal_port) = project.internal_port else {
                continue;
            };

            let host = format!("{}.{}", project.id, domain);
            let upstream = format!("litebin-{}:{}", project.id, internal_port);

            routes.push(json!({
                "match": [{ "host": [host] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": upstream }],
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
        }

        // Add the orchestrator API route so /caddy/ask is reachable by Caddy internally
        routes.push(json!({
            "match": [{ "path": ["/caddy/ask"] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Dashboard: proxy API paths to orchestrator, proxy everything else to dashboard service.
        routes.push(json!({
            "match": [{ "host": [domain] }],
            "handle": [{
                "handler": "subroute",
                "routes": [
                    {
                        "match": [{ "path": ORCHESTRATOR_API_PATHS }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": orchestrator_upstream }],
                            "transport": {
                                "protocol": "http",
                                "read_timeout": "1800s",
                                "write_timeout": "1800s"
                            }
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

        // Catch-all wildcard route for sleeping/unknown apps → waker
        routes.push(json!({
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Error handler: when reverse_proxy returns 502/503/504 for app subdomains,
        // fall back to the orchestrator waker which will restart the container.
        let error_routes = json!({
            "routes": [{
                "match": [{ "host": [format!("*.{}", domain)] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": orchestrator_upstream }]
                }]
            }]
        });

        let config = json!({
            "admin": {
                "listen": "0.0.0.0:2019"
            },
            "apps": {
                "http": {
                    "servers": {
                        "srv0": {
                            "listen": [":80", ":443"],
                            "routes": routes,
                            "errors": error_routes
                        }
                    }
                }
            }
        });

        tracing::info!(
            route_count = routes.len() - 1,
            "syncing caddy config"
        );
        tracing::debug!(config = %serde_json::to_string_pretty(&config).unwrap_or_default(), "caddy config payload");

        let url = format!("{}/load", self.admin_url);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&config)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("caddy /load failed ({}): {}", status, body);
        }

        tracing::info!("caddy config loaded successfully");
        Ok(())
    }

    /// Add a single project route and reload.
    pub async fn add_route(
        &self,
        projects: &[Project],
        domain: &str,
        orchestrator_upstream: &str,
    ) -> anyhow::Result<()> {
        self.sync_routes(projects, domain, orchestrator_upstream)
            .await
    }

    /// Remove a route by resyncing without the removed project.
    pub async fn remove_route(
        &self,
        projects: &[Project],
        domain: &str,
        orchestrator_upstream: &str,
    ) -> anyhow::Result<()> {
        self.sync_routes(projects, domain, orchestrator_upstream)
            .await
    }

    /// Check if Caddy admin API is reachable
    pub async fn ping(&self) -> anyhow::Result<()> {
        let url = format!("{}/config/", self.admin_url);
        self.client.get(&url).send().await?;
        Ok(())
    }
}
