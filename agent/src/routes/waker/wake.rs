use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode, Uri},
};
use serde_json::json;
use std::sync::Arc;

use crate::{AgentState, WakeGuard};
use litebin_common::proxy::{WakeClient, classify_wake_client, is_hop_by_hop};
use litebin_common::wake_wait::{WAKE_WAIT_HEADER, WAKE_WAIT_PATH};

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
            builder.body(Body::from_stream(resp.bytes_stream())).unwrap_or_else(|_| {
                Response::builder().status(StatusCode::BAD_GATEWAY).body(Body::from("Bad gateway")).unwrap()
            })
        }
        Err(e) => {
            tracing::error!(error = %e, upstream = %upstream, "proxy error");
            Response::builder().status(StatusCode::BAD_GATEWAY).body(Body::from("Bad gateway")).unwrap()
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
/// Deliberately no Retry-After: the app is down until a human re-enables it.
fn offline_json_response() -> Response<Body> {
    let body = json!({"error": "offline"}).to_string();
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// Proxy with bounded retries on 502/hung attempts; returns the last 502 once the budget elapses.
async fn proxy_with_retry(
    client: &reqwest::Client,
    method: Method,
    upstream: &str,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
    budget: std::time::Duration,
) -> Response<Body> {
    let pq = uri.path_and_query().map(|pq| pq.as_str());
    let deadline = tokio::time::Instant::now() + budget;
    let mut last = proxy_attempt(client, method.clone(), upstream, pq, headers, &body).await;
    while last.status() == StatusCode::BAD_GATEWAY && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        last = proxy_attempt(client, method.clone(), upstream, pq, headers, &body).await;
    }
    last
}

/// One proxy attempt, capped at 3s so an unresponsive upstream can't pin the handler.
async fn proxy_attempt(
    client: &reqwest::Client,
    method: Method,
    upstream: &str,
    path_and_query: Option<&str>,
    headers: &HeaderMap,
    body: &axum::body::Bytes,
) -> Response<Body> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        proxy_request(client, method, upstream, path_and_query, headers, body.clone()),
    )
    .await
    {
        Ok(resp) => resp,
        Err(_) => Response::builder().status(StatusCode::BAD_GATEWAY).body(Body::from("Bad gateway")).unwrap(),
    }
}

/// Long-poll for the waiting page JS; no guard answers "ready" — the reload re-enters the waker flow.
async fn wake_wait_response(state: &AgentState, subdomain: &str) -> Response<Body> {
    let status = match state.wake_locks.get(subdomain).map(|g| g.clone()) {
        Some(guard) => match guard.wait_for(litebin_common::wake_wait::WAKE_WAIT_POLL_CAP).await {
            Some(true) => "ready",
            Some(false) => "failed",
            None => "timeout",
        },
        None => "ready",
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header(WAKE_WAIT_HEADER, "1")
        .body(Body::from(json!({"status": status}).to_string()))
        .unwrap()
}

/// Upstream for a just-woken single-service project, from Docker discovery.
async fn single_service_upstream(state: &AgentState, project_id: &str) -> Option<String> {
    let containers = state.docker.list_running_litebin_containers().await.ok()?;
    let c = containers.into_iter().find(|c| c.project_id == project_id && c.service_name == "web")?;
    Some(format!("{}:{}", c.container_name, c.internal_port))
}

/// The buffered original request a held browser request proxies once its wake
/// finishes.
struct HeldRequest<'a> {
    method: Method,
    uri: &'a Uri,
    headers: &'a HeaderMap,
    body: axum::body::Bytes,
}

/// Held-request outcome by client class — Document gets the waiting page, Asset holds to its cap, Api gets 503;
/// success proxies the original request (local: container network, remote: agent Caddy).
async fn held_wake_response(
    state: &AgentState,
    subdomain: &str,
    is_multi_service: bool,
    client: WakeClient,
    guard: Arc<WakeGuard>,
    req: HeldRequest<'_>,
) -> Response<Body> {
    let budget = match client {
        WakeClient::Asset => litebin_common::wake_wait::WAKE_ASSET_HOLD_CAP,
        _ => litebin_common::wake_wait::wake_silent_hold(),
    };
    match guard.wait_for(budget).await {
        Some(false) => match client {
            WakeClient::Document => error_page(subdomain),
            _ => starting_json_response(),
        },
        None => match client {
            WakeClient::Document => waiting_page(subdomain),
            _ => starting_json_response(),
        },
        Some(true) => {
            let upstream = if is_multi_service {
                find_public_service_upstream(subdomain)
            } else {
                single_service_upstream(state, subdomain).await
            };
            match upstream {
                Some(upstream) => {
                    let resp = proxy_with_retry(
                        &state.proxy_client,
                        req.method,
                        &upstream,
                        req.uri,
                        req.headers,
                        req.body,
                        std::time::Duration::from_secs(3),
                    )
                    .await;
                    if resp.status() == StatusCode::BAD_GATEWAY && client == WakeClient::Document {
                        return waiting_page(subdomain);
                    }
                    resp
                }
                None => match client {
                    WakeClient::Document => waiting_page(subdomain),
                    _ => starting_json_response(),
                },
            }
        }
    }
}

/// Get the domain from registration state. Returns None if not registered.
fn get_domain(state: &AgentState) -> Option<String> {
    state.registration.read().unwrap().as_ref().map(|r| r.domain.clone())
}

/// Catch-all wake handler for the agent.
/// Extracts the subdomain from the Host header, finds the matching container
/// by name (`litebin-{subdomain}`), and wakes it if stopped.
/// For multi-service projects, health-checks all services (throttled) and proxies.
pub async fn wake(State(state): State<AgentState>, req: Request<Body>) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let headers = parts.headers.clone();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let client = classify_wake_client(&headers);
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();

    let domain = match get_domain(&state) {
        Some(d) => d,
        None => {
            return not_found_page();
        }
    };

    // Extract subdomain from Host header
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");

    let subdomain = match extract_subdomain(host, &domain) {
        Some(s) => s.to_string(),
        None => {
            return not_found_page();
        }
    };

    let is_background =
        state.project_meta.read().unwrap().get(&subdomain).map(|entry| entry.is_background).unwrap_or(false);
    if is_background {
        return not_found_page();
    }

    // Waiting-page long-poll: answer with the wake outcome (or "ready" with no guard).
    if uri.path() == WAKE_WAIT_PATH {
        return wake_wait_response(&state, &subdomain).await;
    }

    // Try to find the container — single-service name first, then multi-service prefix
    let container_id =
        if let Ok(Some(id)) = state.docker.find_container_by_name(&format!("litebin-{}", subdomain)).await {
            id
        } else if let Ok(Some(id)) = state.docker.find_container_by_name(&format!("litebin-{}.", subdomain)).await {
            id
        } else {
            return not_found_page();
        };

    // A Docker observation sidecar does not make a single-image project
    // multi-service. The persisted Compose file is the source of truth.
    let is_multi_service = litebin_common::docker::DockerManager::read_compose(&subdomain).is_some();

    // Check if container is running
    let is_running = state.docker.is_container_running(&container_id).await.unwrap_or(false);

    if is_running && is_multi_service {
        // Wake still in progress: wait on its guard.
        if let Some(guard) = state.wake_locks.get(&subdomain).map(|g| g.clone()) {
            return held_wake_response(
                &state,
                &subdomain,
                true,
                client,
                guard,
                HeldRequest { method, uri: &uri, headers: &headers, body },
            )
            .await;
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

            let oneshot_containers = completed_oneshot_container_names(&subdomain);

            let mut stopped_services = Vec::new();
            for cname in &container_names {
                let Ok(Some(id)) = state.docker.find_container_by_name(cname).await else {
                    continue;
                };
                if state.docker.is_container_running(&id).await.unwrap_or(false) {
                    continue;
                }
                // Completed one-shots (exit 0) are healthy — do not treat as crashed.
                if oneshot_containers.contains(cname)
                    && matches!(state.docker.container_exit_code(&id).await.ok().flatten(), Some(0))
                {
                    continue;
                }
                stopped_services.push(cname.clone());
            }

            if !stopped_services.is_empty() {
                tracing::info!(project = %subdomain, stopped = ?stopped_services, "agent waker: multi-service has crashed services");

                // Check if the public service is among the crashed ones
                let public_upstream = find_public_service_upstream(&subdomain);
                let public_container_name = public_upstream.as_ref().and_then(|u| u.split(':').next()).unwrap_or("");

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
                            Err(e) => {
                                tracing::warn!(project = %subdomain_clone, error = %e, "agent waker: background recovery failed")
                            }
                        }
                    });
                }
            }
        }

        if public_service_up {
            // Public service is healthy — proxy the request to the container
            if let Some(upstream) = find_public_service_upstream(&subdomain) {
                let resp = proxy_request(
                    &state.proxy_client,
                    method,
                    &upstream,
                    uri.path_and_query().map(|pq| pq.as_str()),
                    &headers,
                    body,
                )
                .await;
                if resp.status() == StatusCode::BAD_GATEWAY {
                    return match client {
                        WakeClient::Document => waiting_page(&subdomain),
                        _ => starting_json_response(),
                    };
                }
                return resp;
            }
            tracing::warn!(project = %subdomain, "agent waker: multi-service has no public service upstream, falling through");
        }
        // Public service is down — fall through to wake lock below
    } else if is_running {
        // Running but unrouted: rebuild local Caddy, then proxy straight to the container for every client class.
        if let Err(e) = rebuild_local_caddy(&state).await {
            tracing::error!(project = %subdomain, error = %e, "agent waker: failed to rebuild Caddy");
        }
        if let Some(upstream) = single_service_upstream(&state, &subdomain).await {
            let resp = proxy_with_retry(
                &state.proxy_client,
                method,
                &upstream,
                &uri,
                &headers,
                body,
                std::time::Duration::from_secs(2),
            )
            .await;
            if resp.status() != StatusCode::BAD_GATEWAY {
                return resp;
            }
        }
        return match client {
            WakeClient::Document => waiting_page(&subdomain),
            _ => starting_json_response(),
        };
    }

    // Check auto_start_enabled before waking
    let auto_start = state.project_meta.read().unwrap().get(&subdomain).map(|e| e.auto_start_enabled).unwrap_or(true); // default true if not pushed yet

    if !auto_start {
        return match client {
            WakeClient::Document => offline_page(),
            _ => offline_json_response(),
        };
    }

    // Container is stopped — single-flight wake via Entry API
    let guard = std::sync::Arc::new(WakeGuard::new());

    match state.wake_locks.entry(subdomain.clone()) {
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            // We're the first — insert guard and spawn background wake
            entry.insert(guard.clone());
            let state_clone = state.clone();
            let subdomain_clone = subdomain.clone();
            let container_id_clone = container_id.clone();
            let is_multi_clone = is_multi_service;
            let guard_task = guard.clone();

            tokio::spawn(async move {
                if is_multi_clone {
                    tracing::info!(project_id = %subdomain_clone, "agent wake: multi-service wake");
                    let result = wake_multi_service(&state_clone, &subdomain_clone).await;
                    let success = match result {
                        Ok(()) => {
                            // Not-ready fails the wake; the container stays up and serves once it boots.
                            match find_public_service_upstream(&subdomain_clone) {
                                Some(upstream) => {
                                    let docker = state_clone.docker.clone();
                                    let alive_upstream = upstream.clone();
                                    let ready = litebin_common::wake_wait::readiness_probe(
                                        &state_clone.proxy_client,
                                        &upstream,
                                        litebin_common::wake_wait::WAKE_READINESS_MAX,
                                        || {
                                            let docker = docker.clone();
                                            let alive_upstream = alive_upstream.clone();
                                            async move {
                                                litebin_common::wake_wait::upstream_container_alive(
                                                    &docker,
                                                    &alive_upstream,
                                                )
                                                .await
                                            }
                                        },
                                    )
                                    .await;
                                    tracing::info!(project = %subdomain_clone, upstream = %upstream, ready = ready, "agent wake: readiness probe done");
                                    ready
                                }
                                None => true,
                            }
                        }
                        Err(e) => {
                            tracing::warn!(project_id = %subdomain_clone, error = %e, "agent wake: multi-service failed");
                            false
                        }
                    };
                    guard_task.finish(success);
                    let key = subdomain_clone.clone();
                    let locks = state_clone.wake_locks.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(litebin_common::wake_wait::WAKE_GUARD_RETENTION).await;
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
                        let docker_observe = state_clone
                            .project_meta
                            .read()
                            .ok()
                            .and_then(|meta| meta.get(&subdomain_clone).cloned())
                            .is_some_and(|meta| meta.docker_observe);

                        if env_changed || docker_observe {
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
                                        is_background: false,
                                        image: Some(meta.image.clone()),
                                        internal_port: meta.internal_port,
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

                                    let (new_container_id, port) =
                                        crate::routes::containers::run_single_plan(
                                            &state_clone,
                                            &project,
                                            extra_env,
                                            docker_observe,
                                        )
                                        .await?;
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
                            .start_existing_container(&container_id_clone, "web", false)
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

                    let success = match result {
                        Ok(()) => {
                            tracing::info!(project_id = %subdomain_clone, "agent wake: success");
                            // Readiness: guard success means "app accepting
                            // connections". A not-ready result fails the wake — the
                            // container stays up and serves once it finishes booting.
                            match single_service_upstream(&state_clone, &subdomain_clone).await {
                                Some(upstream) => {
                                    let docker = state_clone.docker.clone();
                                    let alive_upstream = upstream.clone();
                                    let ready = litebin_common::wake_wait::readiness_probe(
                                        &state_clone.proxy_client,
                                        &upstream,
                                        litebin_common::wake_wait::WAKE_READINESS_MAX,
                                        || {
                                            let docker = docker.clone();
                                            let alive_upstream = alive_upstream.clone();
                                            async move {
                                                litebin_common::wake_wait::upstream_container_alive(
                                                    &docker,
                                                    &alive_upstream,
                                                )
                                                .await
                                            }
                                        },
                                    )
                                    .await;
                                    tracing::info!(project = %subdomain_clone, upstream = %upstream, ready = ready, "agent wake: readiness probe done");
                                    ready
                                }
                                None => true,
                            }
                        }
                        Err(e) => {
                            tracing::warn!(project_id = %subdomain_clone, error = %e, "agent wake: failed");
                            false
                        }
                    };
                    guard_task.finish(success);
                }

                // Auto-cleanup after the retention window
                let key = subdomain_clone.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(litebin_common::wake_wait::WAKE_GUARD_RETENTION).await;
                    state_clone.wake_locks.remove(&key);
                });
            });

            held_wake_response(
                &state,
                &subdomain,
                is_multi_service,
                client,
                guard,
                HeldRequest { method, uri: &uri, headers: &headers, body },
            )
            .await
        }
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            // Another wake in progress (or completed) — wait on the same guard.
            let existing = entry.get().clone();
            if existing.is_completed() && !existing.is_success() {
                // Remove old lock so the next request can start a fresh wake
                state.wake_locks.remove(&subdomain);
                return match client {
                    WakeClient::Document => error_page(&subdomain),
                    _ => starting_json_response(),
                };
            }
            if existing.is_completed() {
                state.wake_locks.remove(&subdomain);
            }
            held_wake_response(
                &state,
                &subdomain,
                is_multi_service,
                client,
                existing,
                HeldRequest { method, uri: &uri, headers: &headers, body },
            )
            .await
        }
    }
}

/// Container names for compose one-shot services (`service_completed_successfully`).
fn completed_oneshot_container_names(project_id: &str) -> std::collections::HashSet<String> {
    let Some(yaml) = litebin_common::docker::DockerManager::read_compose(project_id) else {
        return std::collections::HashSet::new();
    };
    let extra_env = crate::routes::containers::read_project_env(project_id);
    let Ok(plan) = litebin_common::compose_run::build_compose_run_plan(&yaml, project_id, &extra_env, None, false)
    else {
        return std::collections::HashSet::new();
    };
    plan.configs
        .iter()
        .filter(|c| c.is_oneshot)
        .map(|c| litebin_common::types::container_name(project_id, &c.service_name, None))
        .collect()
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

fn waiting_page(project_id: &str) -> Response<Body> {
    let html = litebin_common::waker_pages::waiting_page_html(project_id);

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .header("Retry-After", "5")
        .header("X-Robots-Tag", "noindex")
        .body(Body::from(html))
        .unwrap()
}

fn error_page(_project_id: &str) -> Response<Body> {
    let html = litebin_common::waker_pages::error_page_html();

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .header("X-Robots-Tag", "noindex")
        .body(Body::from(html))
        .unwrap()
}

fn offline_page() -> Response<Body> {
    let html = litebin_common::waker_pages::offline_page_html();

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/html")
        .header("X-Robots-Tag", "noindex")
        .body(Body::from(html))
        .unwrap()
}

fn not_found_page() -> Response<Body> {
    let html = litebin_common::waker_pages::not_found_page_html();

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "text/html")
        .header("X-Robots-Tag", "noindex")
        .body(Body::from(html))
        .unwrap()
}
