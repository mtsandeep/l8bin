use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode},
};
use serde_json::json;

use crate::{AgentState, WakeGuard};
use litebin_common::docker::DockerErrorKind;
use litebin_common::proxy::{is_hop_by_hop, wants_json};

use super::caddy::{find_public_service_upstream, rebuild_local_caddy};
use super::multi_service::{report_wake_to_master, wake_multi_service};

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
        if is_hop_by_hop(name.as_str()) {
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
                if is_hop_by_hop(name.as_str()) {
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
            Err(e) => {
                match DockerErrorKind::from_anyhow(&e) {
                    DockerErrorKind::NotFound | DockerErrorKind::Connection => false,
                    _ => {
                        tracing::warn!(subdomain = %subdomain, error = %e, "waker: unexpected error listing containers for multi-service check");
                        false
                    }
                }
            }
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
                Err(e) => {
                    tracing::warn!(subdomain = %subdomain, error = %e, "waker: failed to list containers for health check, skipping");
                    Vec::new()
                }
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
                                if let Err(e) = rebuild_local_caddy(&state_clone).await {
                                    tracing::error!(project = %subdomain_clone, error = %e, "agent waker: failed to rebuild Caddy after recovery");
                                }
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
        if let Err(e) = rebuild_local_caddy(&state).await {
            tracing::error!(project = %subdomain, error = %e, "agent waker: failed to rebuild Caddy");
        }
        return if json { starting_json_response() } else { loading_page(&subdomain) };
    }

    // Check auto_start_enabled before waking
    let auto_start = state
        .project_meta
        .read()
        .unwrap()
        .get(&subdomain)
        .map(|e| e.auto_start_enabled)
        .unwrap_or(true); // default true if not pushed yet

    if !auto_start {
        return if json { offline_json_response() } else { offline_page() };
    }

    // Container is stopped — single-flight wake via Entry API
    let guard = std::sync::Arc::new(WakeGuard {
        notify: tokio::sync::Notify::new(),
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
                        let env_changed = crate::routes::containers::env_has_changed(&subdomain_clone);

                        if env_changed {
                            // Read metadata to recreate without asking orchestrator
                            let meta = crate::routes::containers::read_project_metadata(&subdomain_clone);
                            match meta {
                                Some(meta) => {
                                    tracing::info!(project_id = %subdomain_clone, "agent wake: env changed, recreating container");
                                    let _ = state_clone.docker.remove_by_name(&subdomain_clone).await;

                                    let extra_env = crate::routes::containers::read_project_env(&subdomain_clone);
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
                                        status: litebin_common::types::ProjectStatus::Running,
                                        cmd: meta.cmd.clone(),
                                        memory_limit_mb: meta.memory_limit_mb,
                                        cpu_limit: meta.cpu_limit,
                                        custom_domain: None,
                                        volumes: meta.volumes.as_ref().and_then(|v| litebin_common::types::serialize_volumes(v)),
                                        auto_stop_enabled: false,
                                        auto_stop_timeout_mins: 0,
                                        auto_start_enabled: false,
                                        allow_raw_ports: false,
                                        allow_docker_access: false,
                                        last_active_at: None,
                                        service_count: None,
                                        service_summary: None,
                                        deploy_type: None,
                                        created_at: 0,
                                        updated_at: 0,
                                    };

                                    let config = litebin_common::types::RunServiceConfig::from_project(&project, extra_env);
                                    let (new_container_id, port) = state_clone.docker.run_service_container(&config).await?;
                                    crate::routes::containers::write_env_snapshot(&subdomain_clone);

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

                        // Get the mapped port (non-fatal — 0 if not found)
                        let port = state_clone.docker.inspect_mapped_port(&container_id_clone).await?.unwrap_or(0);
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

fn not_found_page() -> Response<Body> {
    let html = litebin_common::waker_pages::not_found_page_html();

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "text/html")
        .body(Body::from(html))
        .unwrap()
}
