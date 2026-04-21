use async_trait::async_trait;
use serde_json::{json, Value};

use crate::caddy::CaddyClient;

/// Pre-resolved routing info for a single project.
/// The orchestrator builds these by joining project data with node data.
#[derive(Debug, Clone)]
pub struct ProjectRoute {
    pub project_id: String,
    /// Resolved upstream: "litebin-{id}:{port}" (local) or "10.0.1.5:443" (remote via TLS)
    pub upstream: String,
    /// The subdomain host: "{project_id}.{domain}"
    pub subdomain_host: String,
    /// Optional custom domain, e.g. "app.example.com"
    pub custom_domain: Option<String>,
    /// Which node this project runs on (None or "local" = master)
    pub node_id: Option<String>,
    /// Public-facing IP of the node (for DNS A records in cloudflare_dns mode)
    pub node_public_ip: Option<String>,
    /// If set, Caddy rewrites the Host header to this value before proxying.
    /// Used for sleeping custom domain routes so the waker receives the subdomain form.
    pub host_rewrite: Option<String>,
    /// Whether the upstream connection requires TLS (true for remote agent Caddy).
    pub upstream_tls: bool,
    /// Docker-network upstream for direct container access: "litebin-{id}:{port}".
    /// Used by agent Caddy in cloudflare_dns mode to proxy to local containers.
    pub container_upstream: Option<String>,
    /// Custom routing rules (path-based and subdomain-based) for this project.
    pub custom_routes: Vec<ProjectCustomRoute>,
}

/// A custom routing rule for a project (path-based or subdomain-based).
#[derive(Debug, Clone)]
pub struct ProjectCustomRoute {
    pub id: String,
    pub project_id: String,
    pub route_type: String, // "path" or "alias"
    pub path: Option<String>,
    pub subdomain: Option<String>,
    pub upstream: String,
    pub priority: i64,
}

#[derive(Debug, Default)]
pub struct DnsSyncResult {
    pub created: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub errors: usize,
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
        sync_dns: bool,
    ) -> anyhow::Result<()>;

    /// Sync only DNS records (no Caddy config changes). Returns counts.
    /// Default implementation is a no-op for routers that don't manage DNS.
    async fn sync_dns_only(
        &self,
        _projects: &[ProjectRoute],
        _domain: &str,
        _dashboard_subdomain: &str,
        _poke_subdomain: &str,
    ) -> anyhow::Result<DnsSyncResult> {
        Ok(DnsSyncResult::default())
    }
}

// ── Master Proxy Router (Mode A) ──────────────────────────────────────────────

/// Routes all traffic through the master node's Caddy.
/// Handles TLS termination, On-Demand TLS, custom domains, and www redirects.
pub struct MasterProxyRouter {
    caddy: CaddyClient,
    /// Path to CA cert inside the Caddy container (e.g. "/certs/ca.pem").
    /// Used to verify TLS connections to remote agent Caddys.
    ca_cert_path: String,
}

impl MasterProxyRouter {
    pub fn new(caddy: CaddyClient, ca_cert_path: String) -> Self {
        Self { caddy, ca_cert_path }
    }

    fn build_config(
        &self,
        projects: &[ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> Value {
        let logging = crate::heartbeat::caddy_logging_config();
        let mut routes: Vec<Value> = Vec::new();

        for p in projects {
            // 1. Subdomain route: {project_id}.{domain} → upstream
            let mut handle = json!({
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": &p.upstream }],
                "handle_response": [{
                    "match": { "status_code": [502, 503, 504] },
                    "routes": [{
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": orchestrator_upstream }]
                        }]
                    }]
                }]
            });
            if p.upstream_tls && !self.ca_cert_path.is_empty() {
                handle["transport"] = json!({
                    "protocol": "http",
                    "tls": {
                        "server_name": "agent",
                        "root_ca_pem_files": [&self.ca_cert_path]
                    }
                });
                // Preserve original Host header — Caddy 2.11+ auto-rewrites Host
                // to the upstream address when TLS is enabled on the transport.
                handle["headers"] = json!({
                    "request": {
                        "set": { "Host": ["{http.request.host}"] }
                    }
                });
            }
            routes.push(json!({
                "match": [{ "host": [&p.subdomain_host] }],
                "handle": [handle]
            }));

            // 2. Custom domain route + www redirect
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
                    let mut cd_handle = json!({
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": &p.upstream }],
                        "handle_response": [{
                            "match": { "status_code": [502, 503, 504] },
                            "routes": [{
                                "handle": [{
                                    "handler": "reverse_proxy",
                                    "upstreams": [{ "dial": orchestrator_upstream }]
                                }]
                            }]
                        }]
                    });
                    if p.upstream_tls && !self.ca_cert_path.is_empty() {
                        cd_handle["transport"] = json!({
                            "protocol": "http",
                            "tls": {
                                "server_name": "agent",
                                "root_ca_pem_files": [&self.ca_cert_path]
                            }
                        });
                        // Preserve original Host header — Caddy 2.11+ auto-rewrites Host
                        // to the upstream address when TLS is enabled on the transport.
                        cd_handle["headers"] = json!({
                            "request": {
                                "set": { "Host": ["{http.request.host}"] }
                            }
                        });
                    }
                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [cd_handle]
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

            // Custom routes: path-based and subdomain-based
            let mut sorted_custom: Vec<_> = p.custom_routes.iter().collect();
            sorted_custom.sort_by_key(|cr| cr.priority);
            for cr in &sorted_custom {
                match cr.route_type.as_str() {
                    "path" => {
                        // Path-based route on all project hosts
                        let mut hosts = vec![p.subdomain_host.clone()];
                        if let Some(ref cd) = p.custom_domain {
                            hosts.push(cd.clone());
                        }
                        let path = cr.path.as_deref().unwrap_or("/");
                        routes.push(json!({
                            "match": [{ "host": hosts, "path": [path] }],
                            "handle": [{
                                "handler": "reverse_proxy",
                                "upstreams": [{ "dial": &cr.upstream }]
                            }]
                        }));
                    }
                    "subdomain" | "alias" => {
                        // Alias-based: {alias}.{subdomain_host}, {alias}.{custom_domain}, and {alias}.{domain}
                        let alias = cr.subdomain.as_deref().unwrap_or("");
                        let mut hosts = vec![format!("{}.{}", alias, p.subdomain_host)];
                        if let Some(ref cd) = p.custom_domain {
                            hosts.push(format!("{}.{}", alias, cd));
                        }
                        // Domain-level alias: {alias}.{domain} (only for "alias" type)
                        if cr.route_type == "alias" {
                            hosts.push(format!("{}.{}", alias, domain));
                        }
                        routes.push(json!({
                            "match": [{ "host": hosts }],
                            "handle": [{
                                "handler": "reverse_proxy",
                                "upstreams": [{ "dial": &cr.upstream }]
                            }]
                        }));
                    }
                    _ => {}
                }
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
        _sync_dns: bool,
    ) -> anyhow::Result<()> {
        let config = self.build_config(projects, domain, orchestrator_upstream, dashboard_subdomain, poke_subdomain);

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
