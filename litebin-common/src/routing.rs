use async_trait::async_trait;
use serde_json::{json, Value};

use crate::caddy::CaddyClient;

/// Pre-resolved routing info for a single project.
/// The orchestrator builds these by joining project data with node data.
#[derive(Debug, Clone)]
pub struct ProjectRoute {
    pub project_id: String,
    /// Resolved upstream: "host.docker.internal:50123" (local) or "10.0.1.5:50200" (remote)
    pub upstream: String,
    /// The subdomain host: "{project_id}.{domain}"
    pub subdomain_host: String,
    /// Optional custom domain, e.g. "app.example.com"
    pub custom_domain: Option<String>,
    /// Which node this project runs on (None or "local" = master)
    pub node_id: Option<String>,
    /// Public-facing IP of the node (for DNS A records in cloudflare_dns mode)
    pub node_public_ip: Option<String>,
}

#[async_trait]
pub trait RoutingProvider: Send + Sync {
    async fn sync_routes(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> anyhow::Result<()>;
}

// ── Master Proxy Router (Mode A) ──────────────────────────────────────────────

/// Routes all traffic through the master node's Caddy.
/// Handles TLS termination, On-Demand TLS, custom domains, and www redirects.
pub struct MasterProxyRouter {
    caddy: CaddyClient,
}

impl MasterProxyRouter {
    pub fn new(caddy: CaddyClient) -> Self {
        Self { caddy }
    }

    fn build_config(
        projects: &[ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> Value {
        let mut routes: Vec<Value> = Vec::new();

        for p in projects {
            // 1. Subdomain route: {project_id}.{domain} → upstream
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

            // 2. Custom domain route + www redirect
            if let Some(cd) = &p.custom_domain {
                // Route: custom_domain → same upstream
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

                // www redirect: if custom_domain is "app.example.com",
                // redirect "www.app.example.com" → "https://app.example.com{uri}"
                // If custom_domain is "www.app.example.com",
                // redirect "app.example.com" → "https://www.app.example.com{uri}"
                let (redirect_from, canonical) = if cd.starts_with("www.") {
                    let bare = &cd[4..];
                    (bare.to_string(), cd.clone())
                } else {
                    (format!("www.{}", cd), cd.clone())
                };

                routes.push(json!({
                    "match": [{ "host": [redirect_from] }],
                    "handle": [{
                        "handler": "static_response",
                        "status_code": 301,
                        "headers": {
                            "Location": [format!("https://{}{{{{uri}}}}", canonical)]
                        }
                    }]
                }));
            }
        }

        // /caddy/ask route — reachable by Caddy internally for On-Demand TLS validation
        routes.push(json!({
            "match": [{ "path": ["/caddy/ask"] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Dashboard: proxy API paths to orchestrator, everything else to dashboard
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

        // Catch-all wildcard for sleeping/unknown apps → waker
        routes.push(json!({
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": orchestrator_upstream }]
            }]
        }));

        // Error handler for app subdomains
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
                            {
                                "subjects": ["localhost", "*.localhost"],
                                "issuers": [{
                                    "module": "internal"
                                }]
                            },
                            {
                                "on_demand": true
                            }
                        ]
                    }
                }
            }
        })
    }
}

#[async_trait]
impl RoutingProvider for MasterProxyRouter {
    async fn sync_routes(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> anyhow::Result<()> {
        let config = Self::build_config(projects, domain, orchestrator_upstream, dashboard_subdomain, poke_subdomain);

        tracing::info!(
            route_count = projects.len(),
            "syncing caddy config (master proxy)"
        );
        tracing::debug!(config = %serde_json::to_string_pretty(&config).unwrap_or_default(), "caddy config payload");

        let url = format!("{}/load", self.caddy.admin_url());
        let resp = self
            .caddy
            .post_json(&url, &config)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("caddy /load failed ({}): {}", status, body);
        }

        tracing::info!("caddy config loaded successfully");
        Ok(())
    }
}
