use std::sync::Arc;
use tokio::task::JoinSet;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode},
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tokio::sync::Notify;

use crate::{AgentState, WakeGuard};
use litebin_common::docker::DockerManager;

/// Hop-by-hop headers that must not be forwarded when proxying.
const HOP_BY_HOP: &[&str] = &[
    "connection", "transfer-encoding", "upgrade", "keep-alive",
    "proxy-connection", "proxy-authenticate", "proxy-authorization",
    "te", "trailers", "trailer",
];

/// Reverse-proxy a request to a container on the Docker network.
/// Streams the response back to the client.
async fn proxy_request(
    client: &reqwest::Client,
    method: Method,
    upstream: &str,
    path_and_query: Option<&str>,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    let url = format!("http://{}{}", upstream, path_and_query.unwrap_or("/"));

    let mut req = client.request(method, &url);
    for (name, value) in headers.iter() {
        if HOP_BY_HOP.contains(&name.as_str().to_lowercase().as_str()) {
            continue;
        }
        req = req.header(name, value);
    }
    if !body.is_empty() {
        req = req.body(body);
    }

    match req.send().await {
        Ok(resp) => {
            let mut builder = Response::builder().status(resp.status());
            for (name, value) in resp.headers().iter() {
                if HOP_BY_HOP.contains(&name.as_str().to_lowercase().as_str()) {
                    continue;
                }
                builder = builder.header(name, value);
            }
            builder
                .body(Body::from_stream(resp.bytes_stream()))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .body(Body::from("Bad gateway"))
                        .unwrap()
                })
        }
        Err(e) => {
            tracing::error!(error = %e, upstream = %upstream, "proxy error");
            Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from("Bad gateway"))
                .unwrap()
        }
    }
}

/// Check if the client wants JSON (not HTML). Used to return 503+JSON for API clients.
fn wants_json(headers: &HeaderMap) -> bool {
    !headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("text/html"))
        .unwrap_or(false)
}

/// 503 JSON response for API clients while a container is starting.
fn starting_json_response() -> Response<Body> {
    let body = json!({"error": "starting", "retry_after": 5}).to_string();
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "application/json")
        .header("Retry-After", "5")
        .body(Body::from(body))
        .unwrap()
}

/// 503 JSON response for offline/auto-start-disabled projects.
fn offline_json_response() -> Response<Body> {
    let body = json!({"error": "offline", "retry_after": 5}).to_string();
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "application/json")
        .header("Retry-After", "5")
        .body(Body::from(body))
        .unwrap()
}

type HmacSha256 = Hmac<Sha256>;

/// Get the domain from registration state. Returns None if not registered.
fn get_domain(state: &AgentState) -> Option<String> {
    state.registration.read().unwrap().as_ref().map(|r| r.domain.clone())
}

/// Catch-all wake handler for the agent.
/// Extracts the subdomain from the Host header, finds the matching container
/// by name (`litebin-{subdomain}`), and wakes it if stopped.
/// For multi-service projects, health-checks all services (throttled) and proxies.
pub async fn wake(
    State(state): State<AgentState>,
    req: Request<Body>,
) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let headers = parts.headers.clone();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let json = wants_json(&headers);
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();

    let domain = match get_domain(&state) {
        Some(d) => d,
        None => {
            return not_found_page();
        }
    };

    // Extract subdomain from Host header
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let subdomain = match extract_subdomain(host, &domain) {
        Some(s) => s.to_string(),
        None => {
            return not_found_page();
        }
    };

    // Try to find the container — single-service name first, then multi-service prefix
    let container_id = if let Ok(Some(id)) = state.docker.find_container_by_name(&format!("litebin-{}", subdomain)).await {
        id
    } else if let Ok(Some(id)) = state.docker.find_container_by_name(&format!("litebin-{}.", subdomain)).await {
        id
    } else {
        return not_found_page();
    };

    // Detect multi-service project: check if there are multiple containers with the prefix
    let is_multi_service = {
        let prefix = format!("litebin-{}.", subdomain);
        match state.docker.list_containers_by_prefix(&prefix).await {
            Ok(containers) => containers.len() > 1,
            Err(_) => false,
        }
    };

    // Check if container is running
    let is_running = state
        .docker
        .is_container_running(&container_id)
        .await
        .unwrap_or(false);

    if is_running && is_multi_service {
        // If a wake is still in progress (e.g. DNS wait after container start), show loading page
        if state.wake_locks.contains_key(&subdomain) {
            return if json { starting_json_response() } else { loading_page(&subdomain) };
        }

        // Multi-service running: health-check all services (throttled) and proxy to container.
        let should_check = state
            .multi_svc_health_check
            .get(&subdomain)
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(5))
            .unwrap_or(true);

        let mut public_service_up = true;

        if should_check {
            state.multi_svc_health_check.insert(subdomain.clone(), std::time::Instant::now());

            let prefix = format!("litebin-{}.", subdomain);
            let container_names = match state.docker.list_containers_by_prefix(&prefix).await {
                Ok(names) => names,
                Err(_) => Vec::new(),
            };

            let mut stopped_services = Vec::new();
            for cname in &container_names {
                if let Ok(Some(id)) = state.docker.find_container_by_name(cname).await {
                    if !state.docker.is_container_running(&id).await.unwrap_or(false) {
                        stopped_services.push(cname.clone());
                    }
                }
            }

            if !stopped_services.is_empty() {
                tracing::info!(project = %subdomain, stopped = ?stopped_services, "agent waker: multi-service has crashed services");

                // Check if the public service is among the crashed ones
                let public_upstream = find_public_service_upstream(&subdomain);
                let public_container_name = public_upstream.as_ref()
                    .and_then(|u| u.split(':').next())
                    .unwrap_or("");

                let public_down = stopped_services.iter().any(|s| s == public_container_name);

                if public_down {
                    // Public service is down — fall through to wake lock (loading page)
                    public_service_up = false;
                } else {
                    // Non-public services down but public service up — silently recover in background
                    let state_clone = state.clone();
                    let subdomain_clone = subdomain.clone();
                    tokio::spawn(async move {
                        tracing::info!(project = %subdomain_clone, "agent waker: background recovery of degraded services");
                        match wake_multi_service(&state_clone, &subdomain_clone).await {
                            Ok(_) => {
                                let _ = rebuild_local_caddy(&state_clone).await;
                                tracing::info!(project = %subdomain_clone, "agent waker: background recovery succeeded");
                            }
                            Err(e) => tracing::warn!(project = %subdomain_clone, error = %e, "agent waker: background recovery failed"),
                        }
                    });
                }
            }
        }

        if public_service_up {
            // Public service is healthy — proxy the request to the container
            if let Some(upstream) = find_public_service_upstream(&subdomain) {
                let resp = proxy_request(&state.proxy_client, method, &upstream, uri.path_and_query().map(|pq| pq.as_str()), &headers, body).await;
                if resp.status() == StatusCode::BAD_GATEWAY {
                    return if json { starting_json_response() } else { loading_page(&subdomain) };
                }
                return resp;
            }
            tracing::warn!(project = %subdomain, "agent waker: multi-service has no public service upstream, falling through");
        }
        // Public service is down — fall through to wake lock below
    } else if is_running {
        // Single-service running — rebuild local Caddy and return loading page
        let _ = rebuild_local_caddy(&state).await;
        return if json { starting_json_response() } else { loading_page(&subdomain) };
    }

    // Check auto_start_enabled before waking
    let auto_start = state
        .project_meta
        .read()
        .unwrap()
        .get(&subdomain)
        .copied()
        .unwrap_or(true); // default true if not pushed yet (backward compat)

    if !auto_start {
        return if json { offline_json_response() } else { offline_page() };
    }

    // Container is stopped — single-flight wake via Entry API
    let guard = Arc::new(WakeGuard {
        notify: Notify::new(),
        success: std::sync::atomic::AtomicBool::new(false),
        completed: std::sync::atomic::AtomicBool::new(false),
    });

    match state.wake_locks.entry(subdomain.clone()) {
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            // We're the first — insert guard and spawn background wake
            entry.insert(guard.clone());
            let state_clone = state.clone();
            let subdomain_clone = subdomain.clone();
            let container_id_clone = container_id.clone();
            let is_multi_clone = is_multi_service;

            tokio::spawn(async move {
                if is_multi_clone {
                    tracing::info!(project_id = %subdomain_clone, "agent wake: multi-service wake");
                    let result = wake_multi_service(&state_clone, &subdomain_clone).await;
                    match result {
                        Ok(_) => guard.success.store(true, std::sync::atomic::Ordering::Relaxed),
                        Err(e) => {
                            tracing::warn!(project_id = %subdomain_clone, error = %e, "agent wake: multi-service failed");
                            guard.success.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                    guard.notify.notify_waiters();
                    let key = subdomain_clone.clone();
                    let locks = state_clone.wake_locks.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        locks.remove(&key);
                    });
                } else {
                    tracing::info!(
                        project_id = %subdomain_clone,
                        container_id = %container_id_clone,
                        "agent wake: starting container"
                    );

                    let result = async {
                        // Check if .env has changed — if so, recreate to pick up new vars
                        let env_changed = super::containers::env_has_changed(&subdomain_clone);

                        if env_changed {
                            // Read metadata to recreate without asking orchestrator
                            let meta = super::containers::read_project_metadata(&subdomain_clone);
                            match meta {
                                Some(meta) => {
                                    tracing::info!(project_id = %subdomain_clone, "agent wake: env changed, recreating container");
                                    let _ = state_clone.docker.remove_by_name(&subdomain_clone).await;

                                    let extra_env = super::containers::read_project_env(&subdomain_clone);
                                    let project = litebin_common::types::Project {
                                        id: subdomain_clone.clone(),
                                        user_id: String::new(),
                                        name: None,
                                        description: None,
                                        image: Some(meta.image.clone()),
                                        internal_port: Some(meta.internal_port),
                                        mapped_port: None,
                                        container_id: None,
                                        node_id: None,
                                        status: "running".to_string(),
                                        cmd: meta.cmd.clone(),
                                        memory_limit_mb: meta.memory_limit_mb,
                                        cpu_limit: meta.cpu_limit,
                                        custom_domain: None,
                                        volumes: meta.volumes.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                                        auto_stop_enabled: false,
                                        auto_stop_timeout_mins: 0,
                                        auto_start_enabled: false,
                                        last_active_at: None,
                                        service_count: None,
                                        service_summary: None,
                                        created_at: 0,
                                        updated_at: 0,
                                    };

                                    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
                                    let (new_container_id, port) = state_clone.docker.run_service_container(&config).await?;
                                    super::containers::write_env_snapshot(&subdomain_clone);

                                    rebuild_local_caddy(&state_clone).await?;
                                    report_wake_to_master(&state_clone, &subdomain_clone, &new_container_id, port).await;
                                    tracing::info!(project_id = %subdomain_clone, port = %port, "agent wake: container recreated with new env");
                                    return anyhow::Ok(());
                                }
                                None => {
                                    tracing::warn!(project_id = %subdomain_clone, "agent wake: env changed but no metadata.json, falling back to docker start");
                                    // Fall through to docker start below
                                }
                            }
                        }

                        // Fast path: env unchanged, just start the existing container
                        state_clone
                            .docker
                            .start_existing_container(&container_id_clone)
                            .await?;

                        // Wait briefly for port assignment
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                        // Get the mapped port
                        let port = state_clone.docker.inspect_mapped_port(&container_id_clone).await?;
                        tracing::info!(
                            project_id = %subdomain_clone,
                            port = %port,
                            "agent wake: container started"
                        );

                        // Rebuild local Caddy with all running containers
                        rebuild_local_caddy(&state_clone).await?;

                        // Report to master (best-effort, don't block on failure)
                        report_wake_to_master(&state_clone, &subdomain_clone, &container_id_clone, port).await;

                        anyhow::Ok(())
                    }
                    .await;

                    match result {
                        Ok(_) => {
                            tracing::info!(project_id = %subdomain_clone, "agent wake: success");
                            guard.success.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::warn!(project_id = %subdomain_clone, error = %e, "agent wake: failed");
                            guard.success.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }

                guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                guard.notify.notify_waiters();

                // Auto-cleanup after 60s
                let key = subdomain_clone.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    state_clone.wake_locks.remove(&key);
                });
            });

            if json { starting_json_response() } else { loading_page(&subdomain) }
        }
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            // Someone else is already waking (or has completed)
            let existing = entry.get().clone();
            if existing.completed.load(std::sync::atomic::Ordering::Relaxed) {
                let success = existing.success.load(std::sync::atomic::Ordering::Relaxed);
                // Remove old lock so the next request can start a fresh wake
                state.wake_locks.remove(&subdomain);
                if success {
                    if json { starting_json_response() } else { loading_page(&subdomain) }
                } else {
                    error_page(&subdomain)
                }
            } else {
                // Wake still in progress — show loading page
                if json { starting_json_response() } else { loading_page(&subdomain) }
            }
        }
    }
}

fn extract_subdomain<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    // Strip port
    let hostname = host.split(':').next()?;
    let suffix = format!(".{}", domain);
    if hostname.ends_with(&suffix) {
        let sub = &hostname[..hostname.len() - suffix.len()];
        if !sub.is_empty() {
            return Some(sub);
        }
    }
    None
}

/// Find the upstream address for the public service of a multi-service project.
/// Reads compose.yaml from disk, parses it, finds the service marked as public,
/// and returns `{container_name}:{port}`.
fn find_public_service_upstream(project_id: &str) -> Option<String> {
    let compose_yaml = DockerManager::read_compose(project_id)?;
    let extra_env = super::containers::read_project_env(project_id);
    let plan = litebin_common::compose_run::build_compose_run_plan(
        &compose_yaml, project_id, &extra_env, None,
    ).ok()?;

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

    // List all running litebin containers with their ports
    let containers = state.docker.list_running_litebin_containers().await?;

    let config = match state.last_caddy_config.read().unwrap().clone() {
        Some(base) => merge_routes_with_persisted(&base, &containers, &domain),
        None => build_config_from_scratch(
            &containers,
            &domain,
            &state.config.cert_pem,
            &state.config.key_pem,
        ),
    };

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
) -> serde_json::Value {
    let existing_routes = base["apps"]["http"]["servers"]["srv0"]["routes"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // Collect existing non-catch-all routes into a host→route map
    let mut route_map: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
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

    // Build routes: multi-service → wake server, single-service → direct to container
    let wake_server_upstream = "host.docker.internal:8444";
    for (project_id, svc_containers) in &by_project {
        let subdomain_host = format!("{}.{}", project_id, domain);

        if svc_containers.len() > 1 {
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

        let upstream_for_cd = if svc_containers.len() > 1 {
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

    // Build routes array: specific routes + catch-all 502
    let mut routes: Vec<serde_json::Value> = route_map.into_values().collect();

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
) -> serde_json::Value {
    let mut routes: Vec<serde_json::Value> = Vec::new();

    // Group containers by project_id to detect multi-service projects
    let mut by_project: std::collections::HashMap<String, Vec<&litebin_common::docker::RunningContainer>> =
        std::collections::HashMap::new();
    for c in containers {
        by_project.entry(c.project_id.clone()).or_default().push(c);
    }

    let wake_server_upstream = "host.docker.internal:8444";
    for (project_id, svc_containers) in &by_project {
        let host = format!("{}.{}", project_id, domain);
        if svc_containers.len() > 1 {
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
                "certificates": {
                    "load_pem": [{
                        "certificate": cert_pem,
                        "key": key_pem
                    }]
                }
            }
        }
    })
}

/// Build a minimal base Caddy config with just TLS cert and a catch-all 502.
/// Pushed on startup before any containers exist, so the agent Caddy has TLS ready
/// for incoming connections from the master Caddy.
/// Uses inline PEM content (load_pem) so the certs don't need to exist inside the Caddy container.
pub fn build_base_caddy_config(cert_pem: &str, key_pem: &str) -> serde_json::Value {
    let logging = litebin_common::heartbeat::caddy_logging_config();

    json!({
        "admin": { "listen": "0.0.0.0:2019" },
        "logging": logging["logging"],
        "apps": {
            "http": {
                "servers": {
                    "srv0": {
                        "listen": [":80", ":443"],
                        "routes": [{
                            "handle": [{
                                "handler": "static_response",
                                "status_code": 502,
                                "body": "No route found"
                            }]
                        }],
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
    })
}

/// Best-effort report to orchestrator about a successful wake.
/// Fire-and-forget — if master is down, this silently fails.
/// Requests are HMAC-signed so the orchestrator can verify authenticity.
async fn report_wake_to_master(
    state: &AgentState,
    project_id: &str,
    container_id: &str,
    mapped_port: u16,
) {
    let reg = match state.registration.read().unwrap().clone() {
        Some(r) => r,
        None => {
            tracing::debug!(project_id, "skipping wake report: agent not registered");
            return;
        }
    };

    let url = reg.wake_report_url;
    let node_id = &reg.node_id;
    let secret = &reg.secret;

    let timestamp = chrono::Utc::now().timestamp();
    let message = format!("{}\n{}", timestamp, node_id);

    // Compute HMAC-SHA256(secret, "{timestamp}\n{node_id}")
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create HMAC");
            return;
        }
    };
    mac.update(message.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    let body = json!({
        "project_id": project_id,
        "container_id": container_id,
        "mapped_port": mapped_port,
    });

    // Fire-and-forget: use a short timeout and ignore errors
    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Agent-Id", node_id.as_str())
        .header("X-Agent-Timestamp", timestamp.to_string())
        .header("X-Agent-Signature", signature)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(project_id, "wake report accepted by orchestrator");
        }
        Ok(resp) => {
            tracing::debug!(
                project_id,
                status = %resp.status(),
                "wake report rejected by orchestrator"
            );
        }
        Err(e) => {
            tracing::debug!(
                project_id,
                error = %e,
                "wake report failed (orchestrator may be down)"
            );
        }
    }
}

fn loading_page(project_id: &str) -> Response<Body> {
    let html = litebin_common::waker_pages::loading_page_html(project_id);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn error_page(_project_id: &str) -> Response<Body> {
    let html = litebin_common::waker_pages::error_page_html();

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn offline_page() -> Response<Body> {
    let html = litebin_common::waker_pages::offline_page_html();

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
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
        if requested.ends_with(&suffix) || requested == domain {
            return StatusCode::OK;
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

/// Wake a multi-service project: read compose.yaml, parse topological order,
/// start all service containers in dependency order, rebuild Caddy, report to master.
async fn wake_multi_service(state: &AgentState, project_id: &str) -> anyhow::Result<()> {
    // Read compose.yaml from disk
    let compose_yaml = match litebin_common::docker::DockerManager::read_compose(project_id) {
        Some(yaml) => yaml,
        None => {
            anyhow::bail!("no compose.yaml found for multi-service project {}", project_id);
        }
    };

    let extra_env = super::containers::read_project_env(project_id);

    let plan = litebin_common::compose_run::build_compose_run_plan(
        &compose_yaml, project_id, &extra_env, None,
    )?;

    tracing::info!(
        project_id = %project_id,
        services = ?plan.service_order,
        "wake_multi_service: starting services in dependency order"
    );

    // Ensure per-project network
    state.docker.ensure_project_network(project_id, None).await?;

    // Connect Caddy to the project network
    let caddy_container = std::env::var("CADDY_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-caddy".into());
    let project_network = litebin_common::types::project_network_name(project_id, None);
    let _ = state.docker.connect_container_to_network(&caddy_container, &project_network).await;

    // Connect agent to the project network so it can proxy to containers
    let agent_container = std::env::var("AGENT_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-agent".into());
    let _ = state.docker.connect_container_to_network(&agent_container, &project_network).await;

    // Build owned lookup: service_name -> RunServiceConfig
    let configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    let mut public_container_id: Option<String> = None;
    let mut public_mapped_port: Option<u16> = None;
    let any_created = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Start services level by level — parallel within each level
    for level in &plan.service_levels {
        let mut tasks: JoinSet<Result<(String, u16, bool), String>> = JoinSet::new();

        for svc_name in level {
            let run_config = configs_map[svc_name].clone();
            let docker = state.docker.clone();
            let svc = svc_name.clone();
            let is_public = run_config.is_public;
            let pid = project_id.to_string();
            let any_created = any_created.clone();

            tasks.spawn(async move {
                // Check if container already exists and is running
                let cname = litebin_common::types::container_name(&pid, &svc, None);
                if let Ok(Some(existing_id)) = docker.find_container_by_name(&cname).await {
                    if docker.is_container_running(&existing_id).await.unwrap_or(false) {
                        tracing::info!(
                            project_id = %pid,
                            service = %svc,
                            "wake_multi_service: service already running, skipping"
                        );
                        // Return existing container info so public service tracking still works
                        let port = run_config.port.unwrap_or(80) as u16;
                        return Ok((existing_id, port, is_public));
                    }
                    // Container exists but is stopped — just start it (fast path)
                    docker.start_existing_container(&existing_id).await
                        .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;
                    tracing::info!(
                        project_id = %pid,
                        service = %svc,
                        container = %existing_id,
                        "wake_multi_service: started existing stopped container"
                    );
                    let port = run_config.port.unwrap_or(80) as u16;
                    return Ok((existing_id, port, is_public));
                }

                let (container_id, mapped_port) = docker.run_service_container(&run_config).await
                    .map_err(|e| format!("failed to start service '{}': {}", svc, e))?;

                any_created.store(true, std::sync::atomic::Ordering::Relaxed);

                tracing::info!(
                    project_id = %pid,
                    service = %svc,
                    container = %container_id,
                    port = %mapped_port,
                    "wake_multi_service: service created"
                );

                Ok((container_id, mapped_port, is_public))
            });
        }

        // Collect results from this level
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((container_id, mapped_port, is_public))) => {
                    if is_public {
                        public_container_id = Some(container_id.clone());
                        public_mapped_port = Some(mapped_port);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "wake_multi_service: failed to start service");
                    anyhow::bail!("{}", e);
                }
                Err(e) => {
                    tracing::error!(error = %e, "wake_multi_service: service task panicked");
                    anyhow::bail!("service task panicked");
                }
            }
        }
    }

    // Rebuild local Caddy with all running containers
    rebuild_local_caddy(state).await?;

    // Report to master with public service info
    if let (Some(cid), Some(port)) = (public_container_id, public_mapped_port) {
        report_wake_to_master(state, project_id, &cid, port).await;
    }

    tracing::info!(project_id = %project_id, "wake_multi_service: all services started");

    // Wait for Docker DNS to propagate only if we created new containers.
    if any_created.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    Ok(())
}

fn not_found_page() -> Response<Body> {
    let html = litebin_common::waker_pages::not_found_page_html();

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}
