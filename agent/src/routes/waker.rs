use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, Response, StatusCode},
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tokio::sync::Notify;

use crate::{AgentState, WakeGuard};

type HmacSha256 = Hmac<Sha256>;

/// Get the domain from registration state. Returns None if not registered.
fn get_domain(state: &AgentState) -> Option<String> {
    state.registration.read().unwrap().as_ref().map(|r| r.domain.clone())
}

/// Catch-all wake handler for the agent.
/// Extracts the subdomain from the Host header, finds the matching container
/// by name (`litebin-{subdomain}`), and wakes it if stopped.
pub async fn wake(
    State(state): State<AgentState>,
    headers: HeaderMap,
    _req: Request<Body>,
) -> Response<Body> {
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

    let container_name = format!("litebin-{}", subdomain);

    // Try to find the container
    let container_id = match state.docker.find_container_by_name(&container_name).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return not_found_page();
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to look up container");
            return not_found_page();
        }
    };

    // Check if container is running
    let is_running = state
        .docker
        .is_container_running(&container_id)
        .await
        .unwrap_or(false);

    if is_running {
        // Container is running — rebuild local Caddy and return loading page
        let _ = rebuild_local_caddy(&state).await;
        return loading_page(&subdomain);
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
        return offline_page();
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

            tokio::spawn(async move {
                tracing::info!(
                    project_id = %subdomain_clone,
                    container_id = %container_id_clone,
                    "agent wake: starting container"
                );

                let result = async {
                    // Start the container
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
                guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                guard.notify.notify_waiters();

                // Auto-cleanup after 60s
                let key = subdomain_clone.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    state_clone.wake_locks.remove(&key);
                });
            });

            loading_page(&subdomain)
        }
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            // Someone else is already waking (or has completed)
            let existing = entry.get().clone();
            if existing.completed.load(std::sync::atomic::Ordering::Relaxed) {
                let success = existing.success.load(std::sync::atomic::Ordering::Relaxed);
                // Remove old lock so the next request can start a fresh wake
                state.wake_locks.remove(&subdomain);
                if success {
                    loading_page(&subdomain)
                } else {
                    error_page(&subdomain)
                }
            } else {
                // Wake still in progress — show loading page
                loading_page(&subdomain)
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
            &state.config.cert_path,
            &state.config.key_path,
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
    containers: &[(String, u16, u16)],
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

    // Add/update running container routes using Docker network container names.
    // Uses litebin-{project_id}:{internal_port} instead of localhost:{mapped_port}
    // because in production the agent Caddy is a separate container on the Docker network.
    for (project_id, internal_port, _mapped_port) in containers {
        let subdomain_host = format!("{}.{}", project_id, domain);
        let upstream = format!("litebin-{}:{}", project_id, internal_port);
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

        // Upgrade sleeping custom domain routes for this container to running routes.
        // A sleeping custom domain route has headers.request.set.Host = "{project_id}.{domain}".
        // Replace it with a direct proxy to the container (no Host rewrite).
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
        for host in hosts_to_upgrade {
            let upstream = format!("litebin-{}:{}", project_id, internal_port);
            route_map.insert(
                host.clone(),
                json!({
                    "match": [{ "host": [host] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": upstream }]
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
fn build_config_from_scratch(
    containers: &[(String, u16, u16)],
    domain: &str,
    cert_path: &str,
    key_path: &str,
) -> serde_json::Value {
    let mut routes: Vec<serde_json::Value> = Vec::new();

    // Running container routes using Docker network names
    for (project_id, internal_port, _mapped_port) in containers {
        let host = format!("{}.{}", project_id, domain);
        let upstream = format!("litebin-{}:{}", project_id, internal_port);
        routes.push(json!({
            "match": [{ "host": [host] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": upstream }]
            }]
        }));
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
                    "load_files": [{
                        "certificate": cert_path,
                        "key": key_path
                    }]
                },
                "automation": {
                    "policies": [{ "on_demand": true }]
                }
            }
        }
    })
}

/// Build a minimal base Caddy config with just TLS cert and a catch-all 502.
/// Pushed on startup before any containers exist, so the agent Caddy has TLS ready
/// for incoming connections from the master Caddy.
pub fn build_base_caddy_config(cert_path: &str, key_path: &str) -> serde_json::Value {
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
                    "load_files": [{
                        "certificate": cert_path,
                        "key": key_path
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
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta http-equiv="refresh" content="1">
    <title>Starting {}</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .loader {{ text-align: center; }}
        .spinner {{ width: 40px; height: 40px; border: 4px solid #334155; border-top: 4px solid #38bdf8; border-radius: 50%; animation: spin 1s linear infinite; margin: 0 auto 16px; }}
        @keyframes spin {{ 0% {{ transform: rotate(0deg); }} 100% {{ transform: rotate(360deg); }} }}
    </style>
</head>
<body>
    <div class="loader">
        <div class="spinner"></div>
        <p>Starting <strong>{}</strong>...</p>
    </div>
</body>
</html>"#,
        project_id, project_id
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn error_page(_project_id: &str) -> Response<Body> {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <meta http-equiv="refresh" content="30">
    <title>Offline</title>
    <style>
        body { font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }
        .msg { text-align: center; }
        h2 { font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }
        p { color: #64748b; margin: 0; font-size: 0.875rem; }
    </style>
</head>
<body>
    <div class="msg">
        <h2>Failed to start the website</h2>
        <p>Retrying in 30 seconds...</p>
    </div>
</body>
</html>"#;

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn offline_page() -> Response<Body> {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Offline</title>
    <style>
        body { font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }
        .msg { text-align: center; }
        h2 { font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }
        p { color: #64748b; margin: 0; font-size: 0.875rem; }
    </style>
</head>
<body>
    <div class="msg">
        <h2>This website is currently offline</h2>
        <p>Auto-start is disabled!</p>
    </div>
</body>
</html>"#;

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn not_found_page() -> Response<Body> {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Not Found</title>
    <style>
        body { font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }
        .msg { text-align: center; }
        h2 { font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }
        p { color: #64748b; margin: 0; font-size: 0.875rem; }
    </style>
</head>
<body>
    <div class="msg">
        <h2>Project not found</h2>
        <p>This project does not exist or has been removed.</p>
    </div>
</body>
</html>"#;

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}
