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
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Body::from("Agent not configured for routing"))
                .unwrap();
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
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Unknown host"))
                .unwrap();
        }
    };

    let container_name = format!("litebin-{}", subdomain);

    // Try to find the container
    let container_id = match state.docker.find_container_by_name(&container_name).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from(format!(
                    "No container found for {}",
                    subdomain
                )))
                .unwrap();
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to look up container");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Container lookup failed"))
                .unwrap();
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
/// This is autonomous — no master or DB needed. Lists containers via Docker API.
async fn rebuild_local_caddy(state: &AgentState) -> anyhow::Result<()> {
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

    let mut routes: Vec<serde_json::Value> = Vec::new();

    for (project_id, port) in &containers {
        let host = format!("{}.{}", project_id, domain);
        routes.push(json!({
            "match": [{ "host": [host] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": format!("localhost:{}", port) }]
            }]
        }));
    }

    let agent_port = state.config.agent_port;

    // Catch-all for sleeping apps → agent wake handler
    routes.push(json!({
        "match": [{ "host": [format!("*.{}", domain)] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": format!("localhost:{}", agent_port) }]
        }]
    }));

    let error_routes = json!({
        "routes": [{
            "match": [{ "host": [format!("*.{}", domain)] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": format!("localhost:{}", agent_port) }]
            }]
        }]
    });

    let config = json!({
        "admin": { "listen": "0.0.0.0:2019" },
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
                    "policies": [{ "on_demand": true }]
                }
            }
        }
    });

    let url = format!("{}/load", caddy.admin_url());
    let resp = caddy.post_json(&url, &config).await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, "agent caddy /load failed: {}", body);
    } else {
        tracing::info!(containers = containers.len(), "agent caddy config rebuilt");
    }

    Ok(())
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
<html><head>
<meta http-equiv="refresh" content="1">
<title>Starting {0}...</title>
<style>
body {{ font-family: -apple-system, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #0a0a0f; color: #e4e4e7; }}
.container {{ text-align: center; }}
h1 {{ font-size: 1.5rem; margin-bottom: 0.5rem; }}
p {{ color: #71717a; }}
.spinner {{ width: 40px; height: 40px; border: 3px solid #27272a; border-top-color: #22d3ee; border-radius: 50%; animation: spin 0.8s linear infinite; margin: 0 auto 1.5rem; }}
@keyframes spin {{ to {{ transform: rotate(360deg); }} }}
</style>
</head><body>
<div class="container">
<div class="spinner"></div>
<h1>Starting {0}</h1>
<p>Waking up the container...</p>
</div>
</body></html>"#,
        project_id
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}

fn error_page(project_id: &str) -> Response<Body> {
    let html = format!(
        r#"<!DOCTYPE html>
<html><head>
<title>Failed to start {0}</title>
<style>
body {{ font-family: -apple-system, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #0a0a0f; color: #e4e4e7; }}
.container {{ text-align: center; }}
h1 {{ font-size: 1.5rem; color: #f87171; }}
p {{ color: #71717a; }}
</style>
<meta http-equiv="refresh" content="30">
</head><body>
<div class="container">
<h1>Failed to start {0}</h1>
<p>Retrying in 30 seconds...</p>
</div>
</body></html>"#,
        project_id
    );

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}
