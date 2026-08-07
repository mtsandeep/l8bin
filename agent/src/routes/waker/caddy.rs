use axum::{extract::State, http::StatusCode};
use serde_json::json;

use crate::AgentState;
use litebin_common::docker::DockerManager;

/// Route name used to skip access-logging for direct uploads (the token is in the path).
const UPLOAD_ROUTE_NAME: &str = "l8b_direct_upload";

/// The path-based reverse-proxy route that sends `/__l8b_upload/*` to the agent's
/// loopback upload server. Matched by path (not host) so it works for direct
/// uploads to the node's raw IP, which won't match any site host. Must be first.
fn upload_route(upload_upstream: &str) -> serde_json::Value {
    json!({
        "name": UPLOAD_ROUTE_NAME,
        "match": [{ "path": ["/__l8b_upload/*"] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": upload_upstream }]
        }]
    })
}

/// Ensure the server skips access-logging for the upload route (token in path).
fn ensure_upload_log_skip(config: &mut serde_json::Value) {
    let logs = &mut config["apps"]["http"]["servers"]["srv0"]["logs"];
    if logs.is_null() || !logs.is_object() {
        *logs = json!({});
    }
    let skip = &mut logs["skip"];
    if skip.is_null() || !skip.is_array() {
        *skip = json!([]);
    }
    if let Some(arr) = skip.as_array_mut() {
        let has = arr.iter().any(|v| v.as_str() == Some(UPLOAD_ROUTE_NAME));
        if !has {
            arr.push(json!(UPLOAD_ROUTE_NAME));
        }
    }
}

/// Ensure the agent's own certificate (SAN=DNS:agent) is loaded as a manual
/// certificate, so Caddy can serve it for SNI=`agent` without falling back to
/// on-demand issuance. Used by direct uploads (the client sends SNI=`agent` while
/// connecting to the node's raw IP). Idempotent; safely creates intermediate
/// objects (the orchestrator-pushed base may only have `tls.automation`).
pub fn ensure_agent_cert_loaded(config: &mut serde_json::Value, cert_pem: &str, key_pem: &str) {
    // `tls` is always an object in practice, but guard defensively.
    let tls = &mut config["apps"]["tls"];
    if !tls.is_object() {
        *tls = json!({});
    }
    let certificates = &mut tls["certificates"];
    if !certificates.is_object() {
        *certificates = json!({});
    }
    let load_pem = &mut certificates["load_pem"];
    if !load_pem.is_array() {
        *load_pem = json!([]);
    }
    let arr = load_pem.as_array_mut().expect("load_pem is an array");
    let already = arr.iter().any(|e| e["certificate"].as_str() == Some(cert_pem));
    if !already {
        arr.push(json!({ "certificate": cert_pem, "key": key_pem }));
    }
}

/// Address of the agent's own wake/permission server, as reachable from the
/// agent Caddy container. Same address used for the wake reverse-proxy routes.
const AGENT_WAKE_UPSTREAM: &str = "host.docker.internal:8444";

/// Force the on-demand TLS permission endpoint to the agent's own `caddy-ask`
/// (loopback/Docker network) instead of the orchestrator's public HTTP listener.
///
/// The orchestrator-pushed config points `ask` at `http://<master_public_ip>/caddy/ask`,
/// which the master redirects http→https; Caddy's on-demand ask client refuses to
/// follow redirects, so issuance fails. The agent can authorize its own hosted
/// domains locally (project subdomains + custom-domain routes), so it asks itself
/// — no master contact, no redirect, and issuance keeps working if the master is down.
pub fn normalize_ask_endpoint(config: &mut serde_json::Value) {
    // Only touch configs that actually use on-demand TLS.
    let Some(tls) = config.get_mut("apps").and_then(|a| a.get_mut("tls")) else { return };
    let Some(automation) = tls.get_mut("automation") else { return };
    let Some(on_demand) = automation.get_mut("on_demand") else { return };
    let permission_ok = on_demand.get("permission").map(|p| p.is_object()).unwrap_or(false);
    if !permission_ok {
        on_demand["permission"] = json!({});
    }
    let permission = on_demand.get_mut("permission").expect("permission present");
    permission["module"] = json!("http");
    permission["endpoint"] = json!(format!("http://{}/internal/caddy-ask", AGENT_WAKE_UPSTREAM));
}

/// Get the domain from registration state. Returns None if not registered.
fn get_domain(state: &AgentState) -> Option<String> {
    state.registration.read().unwrap().as_ref().map(|r| r.domain.clone())
}

/// Find the upstream address for the public service of a multi-service project.
/// Reads compose.yaml from disk, parses it, finds the service marked as public,
/// and returns `{container_name}:{port}`.
pub(super) fn find_public_service_upstream(project_id: &str) -> Option<String> {
    let compose_yaml = DockerManager::read_compose(project_id)?;
    let extra_env = crate::routes::containers::read_project_env(project_id);
    let plan = litebin_common::compose_run::build_compose_run_plan(&compose_yaml, project_id, &extra_env, None, false).ok()?;

    let public = plan.configs.iter().find(|c| c.is_public)?;
    let port = public.port.unwrap_or(80) as u16;
    let container_name = litebin_common::types::container_name(project_id, &public.service_name, None);
    Some(format!("{}:{}", container_name, port))
}

/// Rebuild the local Caddy config with all currently running litebin containers.
/// Uses the last orchestrator-pushed config as a base (preserving sleeping custom domain
/// routes, TLS config, etc.) and adds/updates routes for running containers from Docker.
pub async fn rebuild_local_caddy(state: &AgentState) -> anyhow::Result<()> {
    let caddy = match state.caddy.as_ref() {
        Some(c) => c,
        None => return Ok(()),
    };

    let domain = match get_domain(state) {
        Some(d) => d,
        None => return Ok(()),
    };

    let upload_upstream = format!("host.docker.internal:{}", state.config.upload_port);

    // List all running litebin containers with their ports
    let mut containers = state.docker.list_running_litebin_containers().await?;
    {
        let project_meta = state.project_meta.read().unwrap();
        containers.retain(|container| {
            !project_meta.get(&container.project_id).map(|entry| entry.is_background).unwrap_or(false)
        });
    }

    let mut config = match state.last_caddy_config.read().unwrap().clone() {
        Some(base) => merge_routes_with_persisted(&base, &containers, &domain, &upload_upstream),
        None => build_config_from_scratch(
            &containers,
            &domain,
            &state.config.cert_pem,
            &state.config.key_pem,
            &upload_upstream,
        ),
    };
    // Always ensure the agent cert is loadable for SNI=`agent` (direct uploads),
    // even when the orchestrator-pushed base only has on-demand automation.
    ensure_agent_cert_loaded(&mut config, &state.config.cert_pem, &state.config.key_pem);
    // Route on-demand TLS permission to the agent's own caddy-ask (not the master's
    // redirect-prone public endpoint).
    normalize_ask_endpoint(&mut config);

    let url = format!("{}/load", caddy.admin_url());
    let resp = caddy.post_json(&url, &config).await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, "agent caddy /load failed: {}", body);
    } else {
        tracing::info!(containers = containers.len(), "agent caddy config rebuilt");
    }

    // Persist the updated config
    {
        let mut guard = state.last_caddy_config.write().unwrap();
        *guard = Some(config.clone());
    }
    crate::save_caddy_config_to_file(&config);

    Ok(())
}

/// Merge running container routes into the orchestrator-pushed config.
/// Preserves sleeping custom domain routes, TLS config, and other orchestrator-managed routes.
fn merge_routes_with_persisted(
    base: &serde_json::Value,
    containers: &[litebin_common::docker::RunningContainer],
    domain: &str,
    upload_upstream: &str,
) -> serde_json::Value {
    let existing_routes = base["apps"]["http"]["servers"]["srv0"]["routes"].as_array().cloned().unwrap_or_default();

    // Collect existing non-catch-all routes into a host->route map
    let mut route_map: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    for route in &existing_routes {
        if let Some(hosts) = route["match"][0]["host"].as_array() {
            for host in hosts {
                if let Some(h) = host.as_str() {
                    if !h.contains('*') {
                        route_map.insert(h.to_string(), route.clone());
                    }
                }
            }
        }
    }

    // Group containers by project_id to detect multi-service projects
    let mut by_project: std::collections::HashMap<String, Vec<&litebin_common::docker::RunningContainer>> =
        std::collections::HashMap::new();
    for c in containers {
        by_project.entry(c.project_id.clone()).or_default().push(c);
    }

    // Build routes: multi-service -> wake server, single-service -> direct to container
    let wake_server_upstream = "host.docker.internal:8444";
    for (project_id, svc_containers) in &by_project {
        let subdomain_host = format!("{}.{}", project_id, domain);

        if DockerManager::read_compose(project_id).is_some() {
            // Multi-service: route to agent wake server (health-checked per-request)
            route_map.insert(
                subdomain_host.clone(),
                json!({
                    "match": [{ "host": [subdomain_host] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": wake_server_upstream }]
                    }]
                }),
            );
        } else {
            // Single-service: direct to container
            let c = &svc_containers[0];
            let upstream = format!("{}:{}", c.container_name, c.internal_port);
            route_map.insert(
                subdomain_host.clone(),
                json!({
                    "match": [{ "host": [subdomain_host] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": upstream }]
                    }]
                }),
            );
        }

        // Upgrade sleeping custom domain routes for this project to running routes.
        let mut hosts_to_upgrade: Vec<String> = Vec::new();
        for (host, route) in &route_map {
            if let Some(set_host) = route
                .pointer("/handle/0/headers/request/set/Host")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
            {
                if set_host == subdomain_host {
                    hosts_to_upgrade.push(host.clone());
                }
            }
        }

        let upstream_for_cd = if DockerManager::read_compose(project_id).is_some() {
            wake_server_upstream.to_string()
        } else {
            format!("{}:{}", svc_containers[0].container_name, svc_containers[0].internal_port)
        };
        for host in hosts_to_upgrade {
            route_map.insert(
                host.clone(),
                json!({
                    "match": [{ "host": [host] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": upstream_for_cd }]
                    }]
                }),
            );
        }
    }

    // Build routes array: upload route first (path match, any host), then specific routes + catch-all 502
    let mut routes: Vec<serde_json::Value> = vec![upload_route(upload_upstream)];
    routes.extend(route_map.into_values());

    // Catch-all returns 502 so master Caddy's handle_response triggers the waker
    routes.push(json!({
        "match": [{ "host": [format!("*.{}", domain)] }],
        "handle": [{
            "handler": "static_response",
            "status_code": 502,
            "body": "No route found"
        }]
    }));

    let error_routes = json!({
        "routes": [{
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "static_response",
                "status_code": 502,
                "body": "No route found"
            }]
        }]
    });

    // Build config from base (preserves TLS, admin, etc.)
    let mut config = base.clone();
    config["apps"]["http"]["servers"]["srv0"]["routes"] = json!(routes);
    config["apps"]["http"]["servers"]["srv0"]["errors"] = error_routes;
    ensure_upload_log_skip(&mut config);
    config
}

/// Build a Caddy config from scratch (no persisted config available).
/// Used on first wake before orchestrator has pushed any config, or on agent startup.
/// Uses inline PEM content (load_pem) so the certs don't need to exist inside the Caddy container.
fn build_config_from_scratch(
    containers: &[litebin_common::docker::RunningContainer],
    domain: &str,
    cert_pem: &str,
    key_pem: &str,
    upload_upstream: &str,
) -> serde_json::Value {
    // Upload route first (path match, any host — works for raw-IP direct uploads).
    let mut routes: Vec<serde_json::Value> = vec![upload_route(upload_upstream)];

    // Group containers by project_id to detect multi-service projects
    let mut by_project: std::collections::HashMap<String, Vec<&litebin_common::docker::RunningContainer>> =
        std::collections::HashMap::new();
    for c in containers {
        by_project.entry(c.project_id.clone()).or_default().push(c);
    }

    let wake_server_upstream = "host.docker.internal:8444";
    for (project_id, svc_containers) in &by_project {
        let host = format!("{}.{}", project_id, domain);
        if DockerManager::read_compose(project_id).is_some() {
            // Multi-service: route to agent wake server (health-checked per-request)
            routes.push(json!({
                "match": [{ "host": [host] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": wake_server_upstream }]
                }]
            }));
        } else {
            // Single-service: direct to container
            let c = &svc_containers[0];
            let upstream = format!("{}:{}", c.container_name, c.internal_port);
            routes.push(json!({
                "match": [{ "host": [host] }],
                "handle": [{
                    "handler": "reverse_proxy",
                    "upstreams": [{ "dial": upstream }]
                }]
            }));
        }
    }

    // Catch-all returns 502 so master Caddy's handle_response triggers the waker
    routes.push(json!({
        "match": [{ "host": [format!("*.{}", domain)] }],
        "handle": [{
            "handler": "static_response",
            "status_code": 502,
            "body": "No route found"
        }]
    }));

    let error_routes = json!({
        "routes": [{
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "static_response",
                "status_code": 502,
                "body": "No route found"
            }]
        }]
    });

    let logging = litebin_common::heartbeat::caddy_logging_config();

    let mut config = json!({
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
                "certificates": {
                    "load_pem": [{
                        "certificate": cert_pem,
                        "key": key_pem
                    }]
                }
            }
        }
    });
    ensure_upload_log_skip(&mut config);
    config
}

/// Build a minimal base Caddy config with just TLS cert and a catch-all 502.
/// Pushed on startup before any containers exist, so the agent Caddy has TLS ready
/// for incoming connections from the master Caddy.
/// Uses inline PEM content (load_pem) so the certs don't need to exist inside the Caddy container.
pub fn build_base_caddy_config(cert_pem: &str, key_pem: &str, upload_upstream: &str) -> serde_json::Value {
    let logging = litebin_common::heartbeat::caddy_logging_config();

    let mut config = json!({
        "admin": { "listen": "0.0.0.0:2019" },
        "logging": logging["logging"],
        "apps": {
            "http": {
                "servers": {
                    "srv0": {
                        "listen": [":80", ":443"],
                        "routes": [
                            // Upload route first (path match, any host).
                            upload_route(upload_upstream),
                            {
                                "handle": [{
                                    "handler": "static_response",
                                    "status_code": 502,
                                    "body": "No route found"
                                }]
                            }
                        ],
                        "logs": {}
                    }
                }
            },
            "tls": {
                "certificates": {
                    "load_pem": [{
                        "certificate": cert_pem,
                        "key": key_pem
                    }]
                }
            }
        }
    });
    ensure_upload_log_skip(&mut config);
    config
}

/// GET /internal/caddy-ask?domain=foo.example.com
/// Permission endpoint for Caddy on-demand TLS.
/// Returns 200 if:
///   - domain is a subdomain of the agent's configured domain (e.g. project-id.l8b.in)
///   - domain has a route in the current Caddy config (covers custom domains pushed by orchestrator)
pub async fn caddy_ask(
    State(state): State<AgentState>,
    axum::extract::Query(params): axum::extract::Query<CaddyAskParams>,
) -> StatusCode {
    let Some(requested) = params.domain else {
        return StatusCode::FORBIDDEN;
    };

    // Check 1: subdomain of the configured domain
    if let Some(domain) = get_domain(&state) {
        let suffix = format!(".{}", domain);
        if let Some(project_id) = requested.strip_suffix(&suffix) {
            let approved =
                state.project_meta.read().unwrap().get(project_id).map(|entry| !entry.is_background).unwrap_or(false);
            if approved {
                return StatusCode::OK;
            }
        }
    }

    // Check 2: domain has a route in the current Caddy config (custom domains)
    if let Some(config) = state.last_caddy_config.read().unwrap().as_ref() {
        if let Some(routes) = config["apps"]["http"]["servers"]["srv0"]["routes"].as_array() {
            for route in routes {
                if let Some(hosts) = route["match"][0]["host"].as_array() {
                    for host in hosts {
                        if let Some(h) = host.as_str() {
                            if h == requested {
                                return StatusCode::OK;
                            }
                        }
                    }
                }
            }
        }
    }

    StatusCode::FORBIDDEN
}

#[derive(serde::Deserialize)]
pub struct CaddyAskParams {
    pub domain: Option<String>,
}
