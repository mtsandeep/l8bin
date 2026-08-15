use litebin_common::caddy::{ORCHESTRATOR_API_PATHS, http_to_https_redirect};
use litebin_common::routing::{ProjectRoute, wake_fallback_handle};
use serde_json::{Value, json};

use super::CloudflareDnsRouter;

impl CloudflareDnsRouter {
    /// Build Caddy config for the master node (local projects + dashboard/API).
    pub(super) fn build_master_caddy_config(
        local_projects: &[&ProjectRoute],
        domain: &str,
        orchestrator_upstream: &str,
        dashboard_subdomain: &str,
        poke_subdomain: &str,
    ) -> Value {
        let logging = litebin_common::heartbeat::caddy_logging_config();
        let mut routes: Vec<Value> = Vec::new();

        if let Some(redirect) = http_to_https_redirect(domain) {
            routes.push(redirect);
        }

        for p in local_projects {
            // Custom routes: path-based and subdomain-based (sorted by priority within
            // this project). Must come BEFORE the catch-all host route.
            let mut sorted_custom: Vec<_> = p.custom_routes.iter().collect();
            sorted_custom.sort_by_key(|cr| cr.priority);
            for cr in &sorted_custom {
                match cr.route_type.as_str() {
                    "path" => {
                        let mut hosts = vec![p.subdomain_host.clone()];
                        if let Some(ref cd) = p.custom_domain {
                            hosts.push(cd.clone());
                        }
                        let path = cr.path.as_deref().unwrap_or("/");
                        // Use handle_response to catch 502/503/504 and proxy
                        // to orchestrator for auto-wake
                        let fallback = wake_fallback_handle(orchestrator_upstream);
                        routes.push(json!({
                            "match": [{ "host": hosts, "path": [path] }],
                            "handle": [{
                                "handler": "reverse_proxy",
                                "upstreams": [{ "dial": &cr.upstream }],
                                "handle_response": fallback
                            }]
                        }));
                    }
                    "subdomain" | "alias" => {
                        let alias = cr.subdomain.as_deref().unwrap_or("");
                        let mut hosts = vec![format!("{}.{}", alias, p.subdomain_host)];
                        if let Some(ref cd) = p.custom_domain {
                            hosts.push(format!("{}.{}", alias, cd));
                        }
                        if cr.route_type == "alias" {
                            hosts.push(format!("{}.{}", alias, domain));
                        }
                        // Add handle_response for auto-wake when upstream is down
                        let fallback = wake_fallback_handle(orchestrator_upstream);
                        routes.push(json!({
                            "match": [{ "host": hosts }],
                            "handle": [{
                                "handler": "reverse_proxy",
                                "upstreams": [{ "dial": &cr.upstream }],
                                "handle_response": fallback
                            }]
                        }));
                    }
                    _ => {}
                }
            }

            // Catch-all subdomain route
            let subdomain_fallback = wake_fallback_handle(orchestrator_upstream);
            routes.push(json!({
                "match": [{ "host": [p.subdomain_host] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": p.upstream }],
                    "handle_response": subdomain_fallback
                }]
            }));

            if let Some(cd) = &p.custom_domain {
                if let Some(ref rewrite) = p.host_rewrite {
                    // Sleeping custom domain: proxy to orchestrator waker with Host rewrite
                    let (www_host, _canonical) = match cd.strip_prefix("www.") {
                        Some(rest) => (rest.to_string(), cd.clone()),
                        None => (format!("www.{}", cd), cd.clone()),
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
                    let cd_fallback = wake_fallback_handle(orchestrator_upstream);
                    routes.push(json!({
                        "match": [{ "host": [cd] }],
                        "handle": [{
                            "handler": "reverse_proxy",
                            "upstreams": [{ "dial": p.upstream }],
                            "handle_response": cd_fallback
                        }]
                    }));

                    let (redirect_from, canonical) = match cd.strip_prefix("www.") {
                        Some(rest) => (rest.to_string(), cd.clone()),
                        None => (format!("www.{}", cd), cd.clone()),
                    };
                    routes.push(json!({
                        "match": [{ "host": [redirect_from] }],
                        "handle": [{
                            "handler": "static_response",
                            "status_code": 301,
                            "headers": { "Location": [format!("https://{}{{uri}}", canonical)] }
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
                        "match": [{ "path": ORCHESTRATOR_API_PATHS }],
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
    pub(super) fn build_agent_caddy_config(
        agent_projects: &[&ProjectRoute],
        domain: &str,
        orchestrator_url: &str,
    ) -> Value {
        let mut routes: Vec<Value> = Vec::new();

        if let Some(redirect) = http_to_https_redirect(domain) {
            routes.push(redirect);
        }

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
                    let (www_host, _canonical) = match cd.strip_prefix("www.") {
                        Some(rest) => (rest.to_string(), cd.clone()),
                        None => (format!("www.{}", cd), cd.clone()),
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

                    let (redirect_from, canonical) = match cd.strip_prefix("www.") {
                        Some(rest) => (rest.to_string(), cd.clone()),
                        None => (format!("www.{}", cd), cd.clone()),
                    };
                    routes.push(json!({
                        "match": [{ "host": [redirect_from] }],
                        "handle": [{
                            "handler": "static_response",
                            "status_code": 301,
                            "headers": { "Location": [format!("https://{}{{uri}}", canonical)] }
                        }]
                    }));
                }
            }

            // Custom routes for agent (only for running projects)
            if p.host_rewrite.is_none() {
                let mut sorted_custom: Vec<_> = p.custom_routes.iter().collect();
                sorted_custom.sort_by_key(|cr| cr.priority);
                for cr in &sorted_custom {
                    match cr.route_type.as_str() {
                        "path" => {
                            let mut hosts = vec![p.subdomain_host.clone()];
                            if let Some(ref cd) = p.custom_domain {
                                hosts.push(cd.clone());
                            }
                            let path = cr.path.as_deref().unwrap_or("/");
                            // Use handle_response to catch 502/503/504 and proxy
                            // to agent wake server for auto-wake
                            let agent_fallback = wake_fallback_handle("litebin-agent:8444");
                            routes.push(json!({
                                "match": [{ "host": hosts, "path": [path] }],
                                "handle": [{
                                    "handler": "reverse_proxy",
                                    "upstreams": [{ "dial": &cr.upstream }],
                                    "handle_response": agent_fallback
                                }]
                            }));
                        }
                        "subdomain" | "alias" => {
                            let alias = cr.subdomain.as_deref().unwrap_or("");
                            let mut hosts = vec![format!("{}.{}", alias, p.subdomain_host)];
                            if let Some(ref cd) = p.custom_domain {
                                hosts.push(format!("{}.{}", alias, cd));
                            }
                            if cr.route_type == "alias" {
                                hosts.push(format!("{}.{}", alias, domain));
                            }
                            // Add handle_response for auto-wake when upstream is down
                            let agent_fallback = wake_fallback_handle("litebin-agent:8444");
                            routes.push(json!({
                                "match": [{ "host": hosts }],
                                "handle": [{
                                    "handler": "reverse_proxy",
                                    "upstreams": [{ "dial": &cr.upstream }],
                                    "handle_response": agent_fallback
                                }]
                            }));
                        }
                        _ => {}
                    }
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
}
