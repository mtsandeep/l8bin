use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use serde_json::json;

use litebin_common::proxy::wants_json;
use litebin_common::types::ProjectStatus;

use crate::AppState;

use super::helpers::{
    handle_down_services, proxy_request, resolve_alias_project, try_acquire_project_lock,
};

fn starting_json_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "5")],
        json!({"error": "starting", "retry_after": 5}).to_string(),
    )
        .into_response()
}

fn loading_page_html(subdomain: &str) -> Html<String> {
    Html(litebin_common::waker_pages::loading_page_html(subdomain))
}

fn error_page_html() -> Html<String> {
    Html(litebin_common::waker_pages::error_page_html())
}

fn not_found_page_html() -> Html<String> {
    Html(litebin_common::waker_pages::not_found_page_html())
}

fn offline_page_html() -> Html<String> {
    Html(litebin_common::waker_pages::offline_page_html())
}

fn not_ready_page_html() -> Html<String> {
    Html(litebin_common::waker_pages::not_ready_page_html())
}

/// Core waker logic — shared by the fallback handler and the subdomain intercept middleware.
pub async fn wake_for_host(
    state: AppState,
    host: &str,
    wants_json: bool,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response {

    let domain_suffix = format!(".{}", state.config.domain);

    let project = if host.ends_with(&domain_suffix) {
        // Subdomain URL (e.g., myapp.l8b.in) — extract project ID
        let subdomain = host.split('.').next().unwrap_or("");
        if subdomain.is_empty() {
            return (StatusCode::NOT_FOUND, not_found_page_html()).into_response();
        }
        match sqlx::query_as::<_, crate::db::models::Project>(
            "SELECT * FROM projects WHERE id = ?",
        )
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
            "SELECT * FROM projects WHERE custom_domain = ?",
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

    // Use project.id as the canonical key for everything (wake locks, display, etc.)
    let project_id = project.id.clone();
    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    // A project record exists, but no deploy artifacts have been staged yet.
    if project.status == ProjectStatus::Pending {
        tracing::info!(project = %project_id, "waker: project pending, refusing auto-start");
        if wants_json {
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
        if wants_json {
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
        return if wants_json {
            starting_json_response()
        } else {
            loading_page_html(&project_id).into_response()
        };
    }

    // Unified running/degraded path for ALL projects
    let is_multi = project.service_count.unwrap_or(1) > 1;
    if project.status == ProjectStatus::Running || project.status == ProjectStatus::Degraded {
        // Remote: no Docker checks possible, return loading page
        if is_remote {
            return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
        }

        // If an operation (deploy, recreate, wake) is in progress, show loading page
        let lock_held = state.project_locks.get(&project_id)
            .map(|s| s.available_permits() == 0)
            .unwrap_or(false);
        if lock_held {
            return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
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
                    if let Some(cid) = container_id {
                        if !state.docker.is_container_running(cid).await.unwrap_or(false) {
                            crashed_services.push(service_name.clone());
                        }
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
            // Single-service: fix port drift on projects table, return loading page
            if !is_multi {
                if let Some(ref container_id) = project.container_id {
                    if state.docker.is_container_running(container_id).await.unwrap_or(true) {
                        if let Ok(Some(actual_port)) = state.docker.inspect_mapped_port(container_id).await {
                            let db_port = project.mapped_port.unwrap_or(0) as u16;
                            if actual_port != db_port {
                                let now = chrono::Utc::now().timestamp();
                                if let Err(e) = sqlx::query(
                                    "UPDATE projects SET mapped_port = ?, updated_at = ? WHERE id = ?",
                                )
                                .bind(actual_port as i64)
                                .bind(now)
                                .bind(&project_id)
                                .execute(&state.db)
                                .await
                                {
                                    tracing::warn!(project_id = %project_id, error = %e, "waker: failed to update mapped_port");
                                }
                                crate::status::sync_single_service_row(&state.db, &project_id, container_id, actual_port as i64).await;
                                tracing::info!(project = %project_id, old = %db_port, new = %actual_port, "waker: port drifted, updated DB");
                            }
                        }
                        let _ = state.route_sync_tx.send(());
                        return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
                    }
                }
            }

            // Multi-service: proxy to public service container
            let public_svc: Option<(String, Option<i64>)> = sqlx::query_as::<_, (String, Option<i64>)>(
                "SELECT service_name, port FROM project_services WHERE project_id = ? AND is_public = 1 AND status = 'running' LIMIT 1",
            )
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some((svc_name, port)) = public_svc {
                let container_name = litebin_common::types::container_name(&project_id, &svc_name, None);
                let upstream = format!("{}:{}", container_name, port.unwrap_or(80) as u16);
                let mut last_resp = proxy_request(&state.proxy_client, method.clone(), &upstream, uri.path_and_query().map(|pq| pq.as_str()), headers, body.clone()).await;
                for _ in 0..2 {
                    if last_resp.status() != StatusCode::BAD_GATEWAY {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    last_resp = proxy_request(&state.proxy_client, method.clone(), &upstream, uri.path_and_query().map(|pq| pq.as_str()), headers, body.clone()).await;
                }
                if last_resp.status() == StatusCode::BAD_GATEWAY {
                    return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
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

            if let Some((svc_name, port, container_id)) = public_svc_any {
                if let Some(ref cid) = container_id {
                    if state.docker.is_container_running(cid).await.unwrap_or(false) {
                        tracing::info!(project = %project_id, service = %svc_name, "waker: public service running but DB stale, syncing status");
                        if let Err(e) = crate::status::transition(&state.db, &project_id, ProjectStatus::Running, &crate::status::ProjectUpdateFields::default(), Some(&[svc_name.clone()])).await {
                            tracing::warn!(project_id = %project_id, error = %e, "waker: failed to sync stale Running status");
                        }
                        let container_name = litebin_common::types::container_name(&project_id, &svc_name, None);
                        let upstream = format!("{}:{}", container_name, port.unwrap_or(80) as u16);
                        let resp = proxy_request(&state.proxy_client, method.clone(), &upstream, uri.path_and_query().map(|pq| pq.as_str()), headers, body.clone()).await;
                        return resp;
                    }
                }
            }

            // Public service truly not running — fall through to wake lock
            tracing::warn!(project = %project_id, "waker: public service not running, falling through");
        }
        // If public down, fall through to wake lock below
    }

    if !project.auto_start_enabled {
        if wants_json {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                json!({"error": "offline", "retry_after": 5}).to_string(),
            )
                .into_response();
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

    // Single-flight dedup: first caller spawns background wake, all get loading page.
    // Uses the unified project_locks semaphore — try_acquire is atomic.
    let permit = match try_acquire_project_lock(&state, &project_id) {
        Some(p) => p,
        None => {
            tracing::info!(project = %project_id, "waker: another operation in progress");
            return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
        }
    };

    let is_stopped = project.status == ProjectStatus::Stopped || project.status == ProjectStatus::Degraded;
    let state_clone = state.clone();
    let project_clone = project.clone();
    let project_id_bg = project_id.clone();

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
                crate::routes::manage::start_services(&state_for_start, &project_clone, crate::routes::manage::StartServicesOpts {
                    force_recreate: false,
                    pull_images: true,
                    force_pull: false,
                    services: None,
                    connect_orchestrator: true,
                    rollback_on_failure: false,
                }).await.map_err(|(s, e)| (s, e).into_response())
            }.boxed()
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(60), wake_fut).await;

        let success = matches!(result, Ok(Ok(())));

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

        // permit dropped here, releasing the project lock
        drop(permit);
    });

    if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() }
}

/// Catch-all fallback handler. Caddy proxies here when no project-specific route matches.
pub async fn wake(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let (parts, body) = req.into_parts();
    let host = parts.headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let json = wants_json(&parts.headers);
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let headers = parts.headers.clone();
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();
    wake_for_host(state, host, json, method, &uri, &headers, body).await
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
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let config = &state.config;
    let dashboard_host = format!("{}.{}", config.dashboard_subdomain, config.domain);
    let poke_host = format!("{}.{}", config.poke_subdomain, config.domain);
    let host_without_port = host.split(':').next().unwrap_or(&host);
    let orchestrator_name = std::env::var("ORCHESTRATOR_CONTAINER_NAME")
        .unwrap_or_else(|_| "litebin-orchestrator".into());

    // Let dashboard, poke, bare domain, and internal container requests pass through
    if host_without_port == config.domain
        || host_without_port == dashboard_host
        || host_without_port == poke_host
        || host_without_port == orchestrator_name
    {
        return next.run(req).await;
    }

    // Everything else is an app request (subdomain or custom domain) — handle via waker
    let json = wants_json(req.headers());
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let headers = parts.headers.clone();
    let body = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap_or_default();
    wake_for_host(state, &host, json, method, &uri, &headers, body).await
}
