use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use litebin_common::proxy::{WakeClient, classify_wake_client};
use litebin_common::types::ProjectStatus;
use litebin_common::wake_wait::{WAKE_WAIT_HEADER, WAKE_WAIT_PATH, WakeGuard};

use crate::AppState;

use super::helpers::{
    handle_down_services, proxy_request, proxy_with_retry, resolve_alias_project, try_acquire_project_lock,
};

fn starting_json_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "5")],
        json!({"error": "starting", "retry_after": 5}).to_string(),
    )
        .into_response()
}

// Transient pages answer 503 (+Retry-After) so fetchers never treat them as content; browsers render the body identically. Not-found: 404.
fn loading_page_html(subdomain: &str, action: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "5"), ("x-robots-tag", "noindex")],
        Html(litebin_common::waker_pages::loading_page_html(subdomain, action)),
    )
        .into_response()
}

fn waiting_page_html(subdomain: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "5"), ("x-robots-tag", "noindex")],
        Html(litebin_common::waker_pages::waiting_page_html(subdomain)),
    )
        .into_response()
}

fn error_page_html() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "30"), ("x-robots-tag", "noindex")],
        Html(litebin_common::waker_pages::error_page_html()),
    )
        .into_response()
}

fn not_found_page_html() -> Response {
    (StatusCode::NOT_FOUND, [("x-robots-tag", "noindex")], Html(litebin_common::waker_pages::not_found_page_html()))
        .into_response()
}

fn offline_page_html() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("x-robots-tag", "noindex")],
        Html(litebin_common::waker_pages::offline_page_html()),
    )
        .into_response()
}

fn not_ready_page_html() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "5"), ("x-robots-tag", "noindex")],
        Html(litebin_common::waker_pages::not_ready_page_html()),
    )
        .into_response()
}

/// Long-poll for the waiting page JS; no guard answers "ready" — the reload re-enters the waker flow.
async fn wake_wait_response(state: &AppState, project_id: &str) -> Response {
    let status = match state.wake_guards.get(project_id).map(|g| g.clone()) {
        Some(guard) => match guard.wait_for(litebin_common::wake_wait::WAKE_WAIT_POLL_CAP).await {
            Some(true) => "ready",
            Some(false) => "failed",
            None => "timeout",
        },
        None => "ready",
    };
    ([(WAKE_WAIT_HEADER.to_string(), "1".to_string())], axum::Json(json!({"status": status}))).into_response()
}

/// Docker-network upstream: `litebin-{id}:{internal_port}` for single-service, the running public service for multi.
async fn resolve_request_upstream(
    state: &AppState,
    project: &crate::db::models::Project,
    path: &str,
) -> Option<String> {
    let routes: Vec<(String, String)> = sqlx::query_as(
        "SELECT path, upstream FROM project_routes WHERE project_id = ? AND route_type = 'path' AND path IS NOT NULL",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // Route paths may be exact ("/api") or glob ("/api/*") — Caddy matcher
    // syntax. Both match the path itself and everything under it.
    let base = |rp: &str| -> String {
        let trimmed = rp.trim_matches('/');
        let no_glob = trimmed.strip_suffix('*').unwrap_or(trimmed).trim_matches('/');
        format!("/{}", no_glob)
    };
    let req = format!("/{}", path.trim_matches('/'));
    if let Some((_, upstream)) = routes
        .iter()
        .filter(|(rp, _)| {
            let b = base(rp);
            req == b || req.starts_with(&format!("{b}/"))
        })
        .max_by_key(|(rp, _)| base(rp).len())
    {
        return Some(upstream.clone());
    }
    resolve_local_upstream(state, project).await
}

async fn all_service_upstreams(state: &AppState, project: &crate::db::models::Project) -> Vec<String> {
    if project.deploy_type != Some(litebin_common::types::DeployType::Compose) {
        if let Some(port) = project.internal_port {
            return vec![format!(
                "{}:{}",
                litebin_common::types::container_name(&project.id, "web", None),
                port as u16
            )];
        }
        return Vec::new();
    }
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT service_name, port FROM project_services WHERE project_id = ? AND status = 'running' AND is_oneshot = 0 AND service_name != ? AND port IS NOT NULL",
    )
    .bind(&project.id)
    .bind(litebin_common::types::DOCKER_PROXY_SERVICE)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|(svc_name, port)| {
            format!(
                "{}:{}",
                litebin_common::types::container_name(&project.id, svc_name, None),
                port.unwrap_or(80) as u16
            )
        })
        .collect()
}

async fn resolve_local_upstream(state: &AppState, project: &crate::db::models::Project) -> Option<String> {
    if project.deploy_type != Some(litebin_common::types::DeployType::Compose) {
        let port = project.internal_port?;
        return Some(format!("{}:{}", litebin_common::types::container_name(&project.id, "web", None), port as u16));
    }
    let svc = sqlx::query_as::<_, (String, Option<i64>)>(
        "SELECT service_name, port FROM project_services WHERE project_id = ? AND is_public = 1 AND status = 'running' LIMIT 1",
    )
    .bind(&project.id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;
    let (svc_name, port) = svc;
    Some(format!(
        "{}:{}",
        litebin_common::types::container_name(&project.id, &svc_name, None),
        port.unwrap_or(80) as u16
    ))
}

/// Held-request outcome by client class — Document gets the waiting page, Asset holds to its cap, Api gets 503;
/// success proxies the original request (local: container network, remote: agent Caddy).
async fn held_wake_response(
    state: &AppState,
    project: &crate::db::models::Project,
    client: WakeClient,
    guard: Arc<WakeGuard>,
    req: HeldRequest<'_>,
) -> Response {
    let (method, uri, headers, body) = (req.method, req.uri, req.headers, req.body);
    let budget = match client {
        WakeClient::Asset => litebin_common::wake_wait::WAKE_ASSET_HOLD_CAP,
        _ => litebin_common::wake_wait::wake_silent_hold(),
    };
    match guard.wait_for(budget).await {
        Some(false) => match client {
            WakeClient::Document => error_page_html(),
            _ => starting_json_response(),
        },
        None => match client {
            WakeClient::Document => waiting_page_html(&project.id),
            _ => starting_json_response(),
        },
        Some(true) => {
            let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);
            if is_remote {
                // Master can't reach agent Docker networks — proxy through the agent's Caddy; unreachable → waiting page.
                return match super::helpers::remote_proxy_response(
                    state,
                    project,
                    method,
                    uri,
                    headers,
                    body,
                    std::time::Duration::from_secs(3),
                )
                .await
                {
                    Some(resp) => resp,
                    None => match client {
                        WakeClient::Document => waiting_page_html(&project.id),
                        _ => starting_json_response(),
                    },
                };
            }
            match resolve_request_upstream(state, project, uri.path()).await {
                Some(upstream) => {
                    let resp = proxy_with_retry(
                        &state.proxy_client,
                        method,
                        uri,
                        headers,
                        body,
                        &upstream,
                        std::time::Duration::from_secs(3),
                    )
                    .await;
                    if resp.status() == StatusCode::BAD_GATEWAY && client == WakeClient::Document {
                        return waiting_page_html(&project.id);
                    }
                    resp
                }
                None => match client {
                    WakeClient::Document => waiting_page_html(&project.id),
                    _ => starting_json_response(),
                },
            }
        }
    }
}

/// The buffered original request a held request proxies once its wake finishes.
struct HeldRequest<'a> {
    method: Method,
    uri: &'a Uri,
    headers: &'a HeaderMap,
    body: axum::body::Bytes,
}

/// Core waker logic — shared by the fallback handler and the subdomain intercept middleware.
pub async fn wake_for_host(
    state: AppState,
    host: &str,
    client: WakeClient,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let domain_suffix = format!(".{}", state.platform.domain());

    let project = if host.ends_with(&domain_suffix) {
        // Subdomain URL (e.g., myapp.l8b.in) — extract project ID
        let subdomain = host.split('.').next().unwrap_or("");
        if subdomain.is_empty() {
            return (StatusCode::NOT_FOUND, not_found_page_html()).into_response();
        }
        match sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
            .bind(subdomain)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                // No project with that ID — check if it's an alias route
                // e.g., "api2.localhost" or "api2.test.localhost"
                let rest = host.strip_suffix(&domain_suffix).unwrap_or("");
                let alias_pid = resolve_alias_project(&state.db, rest).await;
                match alias_pid {
                    Ok(Some(p)) => Some(p),
                    Ok(None) => None,
                    Err(_) => None,
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "waker: db error");
                return (StatusCode::INTERNAL_SERVER_ERROR, not_found_page_html()).into_response();
            }
        }
    } else {
        // Custom domain URL (e.g., app.example.com) — look up by custom_domain
        let host_clean = host.split(':').next().unwrap_or(host);
        match sqlx::query_as::<_, crate::db::models::Project>(
            "SELECT * FROM projects WHERE is_background = 0 AND custom_domain = ?",
        )
        .bind(host_clean)
        .fetch_optional(&state.db)
        .await
        {
            Ok(Some(p)) => Some(p),
            Ok(None) => None,
            Err(e) => {
                tracing::error!(error = %e, "waker: db error (custom_domain lookup)");
                return (StatusCode::INTERNAL_SERVER_ERROR, not_found_page_html()).into_response();
            }
        }
    };

    let project = match project {
        Some(p) => p,
        None => {
            return (StatusCode::NOT_FOUND, not_found_page_html()).into_response();
        }
    };

    if project.is_background {
        return (StatusCode::NOT_FOUND, not_found_page_html()).into_response();
    }

    // Use project.id as the canonical key for everything (wake locks, display, etc.)
    let project_id = project.id.clone();
    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    // Waiting-page long-poll: answer with the wake outcome (or "ready" with no guard).
    if uri.path() == WAKE_WAIT_PATH {
        return wake_wait_response(&state, &project_id).await;
    }

    // A project record exists, but no deploy artifacts have been staged yet.
    if project.status == ProjectStatus::Pending {
        tracing::info!(project = %project_id, "waker: project pending, refusing auto-start");
        if client != WakeClient::Document {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                json!({"error": "pending", "retry_after": 5}).to_string(),
            )
                .into_response();
        }
        return (StatusCode::SERVICE_UNAVAILABLE, not_ready_page_html()).into_response();
    }

    // Staged first deploys stay unconfigured until the user confirms start in the CLI.
    // Opening the URL must not bypass that gate.
    if project.status == ProjectStatus::Unconfigured {
        tracing::info!(project = %project_id, "waker: project unconfigured, refusing auto-start");
        if client != WakeClient::Document {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                json!({"error": "unconfigured", "retry_after": 5}).to_string(),
            )
                .into_response();
        }
        return (StatusCode::SERVICE_UNAVAILABLE, not_ready_page_html()).into_response();
    }

    // Deploy/stop in progress — show loading, do not spawn another wake.
    if matches!(project.status, ProjectStatus::Deploying | ProjectStatus::Stopping) {
        let action = if project.status == ProjectStatus::Stopping { "Stopping" } else { "Deploying" };
        return match client {
            WakeClient::Document => loading_page_html(&project_id, action),
            _ => starting_json_response(),
        };
    }

    // Unified running/degraded path for ALL projects
    let is_multi = project.deploy_type == Some(litebin_common::types::DeployType::Compose);
    if project.status == ProjectStatus::Running || project.status == ProjectStatus::Degraded {
        // Running but unrouted on master Caddy — proxy straight through the agent, no waiting-page bounce.
        if is_remote {
            if let Some(resp) = super::helpers::remote_proxy_response(
                &state,
                &project,
                method,
                uri,
                headers,
                body,
                std::time::Duration::from_secs(2),
            )
            .await
            {
                return resp;
            }
            return match client {
                WakeClient::Document => waiting_page_html(&project_id),
                _ => starting_json_response(),
            };
        }

        // If a wake is in progress, wait on its guard; any other operation
        // (deploy, recreate) holding the lock keeps the loading page.
        let lock_held = state.project_locks.get(&project_id).map(|s| s.available_permits() == 0).unwrap_or(false);
        if lock_held {
            if let Some(guard) = state.wake_guards.get(&project_id).map(|g| g.clone()) {
                return held_wake_response(&state, &project, client, guard, HeldRequest { method, uri, headers, body })
                    .await;
            }
            return match client {
                WakeClient::Document => loading_page_html(&project_id, "Updating"),
                _ => starting_json_response(),
            };
        }

        let mut public_service_up = true;

        // Fast DB-only check: are any services already marked "stopped"?
        let db_stopped: Vec<String> = match sqlx::query_scalar(
            "SELECT service_name FROM project_services WHERE project_id = ? AND status = 'stopped' AND is_oneshot = 0",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "waker: failed to fetch DB-stopped services");
                Vec::new()
            }
        };

        if !db_stopped.is_empty() {
            tracing::info!(project = %project_id, stopped = ?db_stopped, "waker: has DB-stopped services");
            handle_down_services(&state, &project, &project_id, &mut public_service_up).await;
        }

        // Throttled Docker health check: detect crashed containers
        if public_service_up {
            let should_check = state
                .multi_svc_health_check
                .get(&project_id)
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(5))
                .unwrap_or(true);

            if should_check {
                state.multi_svc_health_check.insert(project_id.clone(), std::time::Instant::now());

                let services: Vec<(String, Option<String>)> = match sqlx::query_as(
                    "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND status = 'running' AND container_id IS NOT NULL",
                )
                .bind(&project_id)
                .fetch_all(&state.db)
                .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(project_id = %project_id, error = %e, "waker: failed to fetch running services for crash check");
                        Vec::new()
                    }
                };

                let mut crashed_services = Vec::new();
                for (service_name, container_id) in &services {
                    if let Some(cid) = container_id
                        && !state.docker.is_container_running(cid).await.unwrap_or(false)
                    {
                        crashed_services.push(service_name.clone());
                    }
                }

                if !crashed_services.is_empty() {
                    tracing::info!(project = %project_id, crashed = ?crashed_services, "waker: has crashed containers");

                    for service_name in &crashed_services {
                        if let Err(e) = crate::status::set_service_stopped(&state.db, &project_id, service_name).await {
                            tracing::warn!(project_id = %project_id, service = %service_name, error = %e, "waker: failed to set crashed service stopped");
                        }
                    }

                    handle_down_services(&state, &project, &project_id, &mut public_service_up).await;
                }
            }
        }

        if public_service_up {
            // Single-service: fix port drift, then proxy straight to the container (waker reached = stale Caddy route).
            if !is_multi
                && let Some(ref container_id) = project.container_id
                && state.docker.is_container_running(container_id).await.unwrap_or(true)
            {
                if let Ok(Some(actual_port)) = state.docker.inspect_mapped_port(container_id).await {
                    let db_port = project.mapped_port.unwrap_or(0) as u16;
                    if actual_port != db_port {
                        let now = chrono::Utc::now().timestamp();
                        if let Err(e) = sqlx::query("UPDATE projects SET mapped_port = ?, updated_at = ? WHERE id = ?")
                            .bind(actual_port as i64)
                            .bind(now)
                            .bind(&project_id)
                            .execute(&state.db)
                            .await
                        {
                            tracing::warn!(project_id = %project_id, error = %e, "waker: failed to update mapped_port");
                        }
                        crate::status::sync_single_service_row(
                            &state.db,
                            &project_id,
                            container_id,
                            actual_port as i64,
                        )
                        .await;
                        tracing::info!(project = %project_id, old = %db_port, new = %actual_port, "waker: port drifted, updated DB");
                    }
                }
                let _ = state.route_sync_tx.send(());
                if let Some(port) = project.internal_port.map(|p| p as u16) {
                    let upstream =
                        format!("{}:{}", litebin_common::types::container_name(&project_id, "web", None), port);
                    let resp = proxy_with_retry(
                        &state.proxy_client,
                        method,
                        uri,
                        headers,
                        body,
                        &upstream,
                        std::time::Duration::from_secs(2),
                    )
                    .await;
                    if resp.status() != StatusCode::BAD_GATEWAY {
                        return resp;
                    }
                }
                return match client {
                    WakeClient::Document => waiting_page_html(&project_id),
                    _ => starting_json_response(),
                };
            }

            // Multi-service: proxy to the request's upstream (custom path routes win, else public)
            let public_svc = resolve_request_upstream(&state, &project, uri.path()).await;

            if let Some(upstream) = public_svc {
                let upstream = upstream.clone();
                let mut last_resp = proxy_request(
                    &state.proxy_client,
                    method.clone(),
                    &upstream,
                    uri.path_and_query().map(|pq| pq.as_str()),
                    headers,
                    body.clone(),
                )
                .await;
                for _ in 0..2 {
                    if last_resp.status() != StatusCode::BAD_GATEWAY {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    last_resp = proxy_request(
                        &state.proxy_client,
                        method.clone(),
                        &upstream,
                        uri.path_and_query().map(|pq| pq.as_str()),
                        headers,
                        body.clone(),
                    )
                    .await;
                }
                if last_resp.status() == StatusCode::BAD_GATEWAY {
                    return match client {
                        WakeClient::Document => waiting_page_html(&project_id),
                        _ => starting_json_response(),
                    };
                }
                return last_resp;
            }

            // No public service found with status='running' in DB — check Docker (stale DB)
            let public_svc_any: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as::<_, (String, Option<i64>, Option<String>)>(
                "SELECT service_name, port, container_id FROM project_services WHERE project_id = ? AND is_public = 1 LIMIT 1",
            )
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some((svc_name, port, container_id)) = public_svc_any
                && let Some(ref cid) = container_id
                && state.docker.is_container_running(cid).await.unwrap_or(false)
            {
                tracing::info!(project = %project_id, service = %svc_name, "waker: public service running but DB stale, syncing status");
                if let Err(e) = crate::status::transition(
                    &state.db,
                    &project_id,
                    ProjectStatus::Running,
                    &crate::status::ProjectUpdateFields::default(),
                    Some(std::slice::from_ref(&svc_name)),
                )
                .await
                {
                    tracing::warn!(project_id = %project_id, error = %e, "waker: failed to sync stale Running status");
                }
                let container_name = litebin_common::types::container_name(&project_id, &svc_name, None);
                let upstream = format!("{}:{}", container_name, port.unwrap_or(80) as u16);
                let resp = proxy_request(
                    &state.proxy_client,
                    method.clone(),
                    &upstream,
                    uri.path_and_query().map(|pq| pq.as_str()),
                    headers,
                    body.clone(),
                )
                .await;
                return resp;
            }

            // Public service truly not running — fall through to wake lock
            tracing::warn!(project = %project_id, "waker: public service not running, falling through");
        }
        // If public down, fall through to wake lock below
    }

    if !project.auto_start_enabled {
        if client != WakeClient::Document {
            // No Retry-After: down until a human re-enables it.
            return (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "offline"}).to_string()).into_response();
        }
        return (StatusCode::SERVICE_UNAVAILABLE, offline_page_html()).into_response();
    }

    // Check for recent wake failure — show error page instead of retrying
    if let Some(failed_at) = state.wake_failures.get(&project_id) {
        if failed_at.elapsed() < std::time::Duration::from_secs(60) {
            return error_page_html().into_response();
        }
        state.wake_failures.remove(&project_id);
    }

    // Single-flight: first caller spawns the wake + guard; others hold on the guard.
    let guard = Arc::new(WakeGuard::new());
    let permit = match try_acquire_project_lock(&state, &project_id) {
        Some(p) => p,
        None => {
            tracing::info!(project = %project_id, "waker: another operation in progress");
            if let Some(existing) = state.wake_guards.get(&project_id).map(|g| g.clone()) {
                return held_wake_response(
                    &state,
                    &project,
                    client,
                    existing,
                    HeldRequest { method, uri, headers, body },
                )
                .await;
            }
            return match client {
                WakeClient::Document => loading_page_html(&project_id, "Updating"),
                _ => starting_json_response(),
            };
        }
    };

    // Publish the guard before yielding so concurrent requests find it.
    state.wake_guards.insert(project_id.clone(), guard.clone());

    let is_stopped = project.status == ProjectStatus::Stopped || project.status == ProjectStatus::Degraded;
    let state_clone = state.clone();
    let project_clone = project.clone();
    let project_probe = project.clone(); // wake_fut's local branch moves project_clone
    let project_id_bg = project_id.clone();
    let guard_task = guard.clone();

    tracing::info!(project = %project_id, host = %host, "waker: spawning background wake");

    tokio::spawn(async move {
        use futures_util::FutureExt;

        let wake_fut = if is_remote {
            if is_stopped {
                super::helpers::start_stopped_container(&state_clone, &project_clone).boxed()
            } else {
                super::helpers::restart_crashed_container(&state_clone, &project_clone).boxed()
            }
        } else {
            let state_for_start = state_clone.clone();
            async move {
                crate::routes::manage::start_services(
                    &state_for_start,
                    &project_clone,
                    crate::routes::manage::StartServicesOpts {
                        force_recreate: false,
                        pull_images: true,
                        force_pull: false,
                        services: None,
                        connect_orchestrator: true,
                        rollback_on_failure: false,
                        skip_dns_wait: true,
                    },
                )
                .await
                .map_err(|(s, e)| (s, e).into_response())
            }
            .boxed()
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(60), wake_fut).await;

        let mut success = matches!(result, Ok(Ok(())));

        // Readiness: not-ready fails the wake; the container stays up and serves later. Remote gets this via the agent ready flag.
        if success && !is_remote {
            let upstreams = all_service_upstreams(&state_clone, &project_probe).await;
            if !upstreams.is_empty() {
                let docker = state_clone.docker.clone();
                let probe_set = upstreams.clone();
                let ready = litebin_common::wake_wait::readiness_probe_all(
                    &state_clone.proxy_client,
                    &probe_set,
                    litebin_common::wake_wait::WAKE_READINESS_MAX,
                    || {
                        let docker = docker.clone();
                        let probe_set = probe_set.clone();
                        async move { litebin_common::wake_wait::all_upstreams_alive(&docker, &probe_set).await }
                    },
                )
                .await;
                tracing::info!(project = %project_id_bg, count = upstreams.len(), ready = ready, "waker: wake readiness probe done");
                if !ready {
                    success = false;
                }
            }
        }

        if success {
            let _ = state_clone.route_sync_tx.send(());
        } else {
            if result.is_err() {
                tracing::error!(project = %project_id_bg, "waker: background wake timed out");
            } else {
                tracing::error!(project = %project_id_bg, "waker: background wake failed");
            }
            // Track failure so subsequent requests see error page instead of infinite loading
            state_clone.wake_failures.insert(project_id_bg.clone(), std::time::Instant::now());
            let failures = state_clone.wake_failures.clone();
            let pid = project_id_bg.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                failures.remove(&pid);
            });
        }

        guard_task.finish(success);

        // Retire the guard after the retention window; identity-checked so a newer guard survives.
        let guards = state_clone.wake_guards.clone();
        let g = guard_task.clone();
        let pid = project_id_bg.clone();
        tokio::spawn(async move {
            tokio::time::sleep(litebin_common::wake_wait::WAKE_GUARD_RETENTION).await;
            guards.remove_if(&pid, |_, v| Arc::ptr_eq(v, &g));
        });

        // permit dropped here, releasing the project lock
        drop(permit);
    });

    held_wake_response(&state, &project, client, guard, HeldRequest { method, uri, headers, body }).await
}

/// Catch-all for requests that match no API route. Dashboard-host and bare-domain
/// traffic is proxied to the dashboard SPA — the orchestrator is the sole router
/// for the dashboard host. Anything else falls through to the waker.
pub async fn wake(State(state): State<AppState>, req: axum::http::Request<axum::body::Body>) -> Response {
    let (parts, body) = req.into_parts();
    let host = parts.headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let client = classify_wake_client(&parts.headers);
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let headers = parts.headers.clone();
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();

    // Machine-shaped misses answer in JSON, never the SPA.
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        return unknown_api_route(&method, uri.path());
    }

    let snap = state.platform.snapshot();
    let host_without_port = host.split(':').next().unwrap_or("");
    let dashboard_host = format!("{}.{}", snap.dashboard_subdomain, snap.domain);
    if host_without_port == dashboard_host
        || host_without_port == snap.domain
        || (snap.domain == "localhost" && host_without_port == "127.0.0.1")
    {
        // The SPA serves GET/HEAD only — anything else here is a missed API call.
        if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
            return unknown_api_route(&method, uri.path());
        }
        return proxy_request(
            &state.proxy_client,
            method,
            &state.config.dashboard_upstream,
            uri.path_and_query().map(|pq| pq.as_str()),
            &headers,
            body,
        )
        .await;
    }

    wake_for_host(state, host, client, method, &uri, &headers, body).await
}

/// JSON 404 for requests that look like API traffic but matched no route.
fn unknown_api_route(method: &axum::http::Method, path: &str) -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "ok": false,
            "error": {
                "message": format!("unknown API route: {method} {path}"),
                "hint": "endpoints are root-relative (a /api/ prefix is stripped) — see /openapi.json and /llms.txt"
            }
        })),
    )
        .into_response()
}

/// Middleware that intercepts requests for app subdomains BEFORE axum's route matcher.
/// Without this, a GET to `/auth/login` on an app subdomain would match the orchestrator's
/// POST-only `/auth/login` route and return 405 — the fallback never runs when a path
/// matches but the method doesn't.
pub async fn waker_intercept(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    let host = req.headers().get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();

    let snap = state.platform.snapshot();
    let dashboard_host = format!("{}.{}", snap.dashboard_subdomain, snap.domain);
    let poke_host = format!("{}.{}", snap.poke_subdomain, snap.domain);
    let host_without_port = host.split(':').next().unwrap_or(&host);
    let orchestrator_name =
        std::env::var("ORCHESTRATOR_CONTAINER_NAME").unwrap_or_else(|_| "litebin-orchestrator".into());

    // Let dashboard, poke, bare domain, and internal container requests pass through
    if host_without_port == snap.domain
        || host_without_port == dashboard_host
        || host_without_port == poke_host
        || host_without_port == orchestrator_name
    {
        return next.run(req).await;
    }

    // Everything else is an app request (subdomain or custom domain) — handle via waker
    let client = classify_wake_client(req.headers());
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let headers = parts.headers.clone();
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();
    wake_for_host(state, &host, client, method, &uri, &headers, body).await
}
