use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use futures_util::FutureExt;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;
use litebin_common::types::Node;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

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
) -> Response {
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
                .body(axum::body::Body::from_stream(resp.bytes_stream()))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .body(axum::body::Body::from("Bad gateway"))
                        .unwrap()
                })
        }
        Err(e) => {
            tracing::error!(error = %e, upstream = %upstream, "proxy error");
            Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(axum::body::Body::from("Bad gateway"))
                .unwrap()
        }
    }
}

/// Check if the client should get 503+JSON instead of the HTML loading page.
/// Returns true for API clients (no Accept: text/html) and known bots (Googlebot, etc.)
/// so they don't index the loading spinner or retry endlessly.
fn wants_json(headers: &HeaderMap) -> bool {
    if !headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("text/html"))
        .unwrap_or(false)
    {
        return true;
    }
    // Known bots/crawlers send Accept: text/html but should get 503, not the spinner
    if let Some(ua) = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()) {
        let ua = ua.to_lowercase();
        if ua.contains("bot") || ua.contains("crawler") || ua.contains("spider") {
            return true;
        }
    }
    false
}

/// 503 JSON response for API clients while a container is starting.
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

/// Try to acquire the per-project lock. Returns None if another operation is in progress.
fn try_acquire_project_lock(state: &AppState, project_id: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let semaphore: Arc<Semaphore> = state.project_locks
        .entry(project_id.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    semaphore.clone().try_acquire_owned().ok()
}

/// Recreate a container on a remote agent (no image pull).
async fn remote_recreate(
    state: &AppState,
    project: &crate::db::models::Project,
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(), Response> {
    let image = project.image.as_deref()
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "no image").into_response())?;
    let internal_port = project.internal_port
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "no port configured").into_response())?;

    let resp = client
        .post(format!("{}/containers/recreate", base_url))
        .json(&json!({
            "image": image,
            "internal_port": internal_port,
            "project_id": project.id,
            "cmd": project.cmd,
            "memory_limit_mb": project.memory_limit_mb,
            "cpu_limit": project.cpu_limit,
            "volumes": project.volumes.as_ref().and_then(|v| serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(v).ok()),
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, project = %project.id, "waker: recreate failed to reach agent");
            (StatusCode::SERVICE_UNAVAILABLE, "agent unreachable").into_response()
        })?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::error!(project = %project.id, "waker: recreate failed: {}", body);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to recreate container").into_response());
    }

    let result: serde_json::Value = resp.json().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("bad response: {e}")).into_response())?;
    let new_container_id = result["container_id"].as_str().unwrap_or("").to_string();
    let mapped_port = result["mapped_port"].as_u64().map(|p| p as u16);

    let now = chrono::Utc::now().timestamp();
    if let Some(port) = mapped_port {
        let _ = sqlx::query(
            "UPDATE projects SET status = 'running', container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&new_container_id)
        .bind(port as i64)
        .bind(now)
        .bind(now)
        .bind(&project.id)
        .execute(&state.db)
        .await;
    } else {
        let _ = sqlx::query(
            "UPDATE projects SET status = 'running', container_id = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&new_container_id)
        .bind(now)
        .bind(now)
        .bind(&project.id)
        .execute(&state.db)
        .await;
    }

    Ok(())
}

/// Start all services of a multi-service project. Delegates to the unified `start_services`.
async fn start_multi_service(state: &AppState, project: &crate::db::models::Project) -> Result<(), Response> {
    crate::routes::manage::start_services(state, project, crate::routes::manage::StartServicesOpts {
        force_recreate: false,
        pull_images: true,
        services: None,
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await.map_err(|(s, e)| (s, e).into_response())
}

async fn start_stopped_container(state: &AppState, project: &crate::db::models::Project) -> Result<(), Response> {
    let subdomain = &project.id;
    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    if is_remote {
        let node_id = project.node_id.as_deref().unwrap().to_string();

        let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, node_id = %node_id, "waker: node client unavailable");
                return Err((StatusCode::SERVICE_UNAVAILABLE, "node unavailable").into_response());
            }
        };

        let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) => n,
            Ok(None) => return Err((StatusCode::SERVICE_UNAVAILABLE, "node not found").into_response()),
            Err(e) => {
                tracing::error!(error = %e, "waker: db error fetching node");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response());
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        // Use the smart start endpoint — agent will compare .env hashes and
        // recreate only if env has changed since last injection.
        let container_id = project.container_id.as_deref().unwrap_or("");
        let resp = match client
            .post(&format!("{}/containers/start", base_url))
            .json(&json!({
                "container_id": container_id,
                "project_id": subdomain,
                "image": project.image,
                "internal_port": project.internal_port,
                "cmd": project.cmd,
                "memory_limit_mb": project.memory_limit_mb,
                "cpu_limit": project.cpu_limit,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, project = %subdomain, "waker: failed to call agent start");
                return Err((StatusCode::SERVICE_UNAVAILABLE, "agent unreachable").into_response());
            }
        };

        if resp.status().is_success() {
            let result: serde_json::Value = resp.json().await.unwrap_or_default();
            let mapped_port = result["mapped_port"].as_u64().map(|p| p as u16);

            let now = chrono::Utc::now().timestamp();
            if let Some(port) = mapped_port {
                let _ = sqlx::query(
                    "UPDATE projects SET status = 'running', mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
                )
                .bind(port as i64)
                .bind(now)
                .bind(now)
                .bind(&subdomain)
                .execute(&state.db)
                .await;
            } else {
                let _ = sqlx::query(
                    "UPDATE projects SET status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?",
                )
                .bind(now)
                .bind(now)
                .bind(&subdomain)
                .execute(&state.db)
                .await;
            }
            return Ok(());
        }

        // Start failed — container may have been pruned. Fall back to recreate.
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(project = %subdomain, body = %body, "waker: agent start failed, trying recreate");
        return remote_recreate(state, project, &client, &base_url).await;
    } else {
        // Local path
        let is_multi = project.service_count.unwrap_or(1) > 1;

        if is_multi {
            // Multi-service: start all services from compose.yaml
            return start_multi_service(state, project).await;
        }

        // Single-service: check if env changed to decide start vs recreate
        let env_changed = crate::routes::manage::local_env_has_changed(&subdomain);

        if !env_changed {
            // Fast path: env unchanged, try docker start on existing container
            if let Some(ref container_id) = project.container_id {
                match state.docker.start_existing_container(container_id).await {
                    Ok(()) => {
                        let now = chrono::Utc::now().timestamp();
                        let _ = sqlx::query(
                            "UPDATE projects SET status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?",
                        )
                        .bind(now)
                        .bind(now)
                        .bind(&subdomain)
                        .execute(&state.db)
                        .await;
                        tracing::info!(project = %subdomain, "waker: started existing container (env unchanged)");
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(project = %subdomain, error = %e, "waker: docker start failed, falling back to recreate");
                    }
                }
            }
        }

        // Recreate: env changed or docker start failed
        let project_clone = {
            let mut p = project.clone();
            p.container_id = None;
            p.mapped_port = None;
            p
        };

        if let Some(ref old_cid) = project.container_id {
            let _ = state.docker.remove_container(old_cid).await;
        }

        let extra_env = crate::routes::manage::read_local_project_env(&subdomain);
        let config = litebin_common::types::RunServiceConfig::from_project(&project_clone, extra_env);
        let (new_container_id, new_mapped_port) = match state.docker.run_service_container(&config).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, project = %subdomain, "waker: failed to create container");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to create container: {e}"),
                )
                    .into_response());
            }
        };

        crate::routes::manage::write_local_env_snapshot(&subdomain);

        let now = chrono::Utc::now().timestamp();
        let _ = sqlx::query(
            "UPDATE projects SET status = 'running', container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&new_container_id)
        .bind(new_mapped_port as i64)
        .bind(now)
        .bind(now)
        .bind(&subdomain)
        .execute(&state.db)
        .await;
    }

    Ok(())
}

async fn restart_crashed_container(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<(), Response> {
    let subdomain = &project.id;
    let is_remote = project.node_id.as_deref().map(|n| n != "local").unwrap_or(false);

    if is_remote {
        let node_id = project.node_id.as_deref().unwrap().to_string();

        let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, node_id = %node_id, "waker: node client unavailable");
                return Err((StatusCode::SERVICE_UNAVAILABLE, "node unavailable").into_response());
            }
        };

        let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) => n,
            Ok(None) => return Err((StatusCode::SERVICE_UNAVAILABLE, "node not found").into_response()),
            Err(e) => {
                tracing::error!(error = %e, "waker: db error fetching node");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response());
            }
        };

        let base_url = agent_base_url(&state.config, &node);
        tracing::info!(project = %subdomain, "waker: remote container down despite DB=running, recreating");
        return remote_recreate(state, project, &client, &base_url).await;
    }

    // Local path
    let is_multi = project.service_count.unwrap_or(1) > 1;

    if is_multi {
        // Multi-service: start all services from compose.yaml
        // run_service_container handles idempotent cleanup internally
        return start_multi_service(state, project).await;
    }

    let Some(ref container_id) = project.container_id else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "no container to restart").into_response());
    };

    let actually_running = state
        .docker
        .is_container_running(container_id)
        .await
        .unwrap_or(false);

    if actually_running {
        return Ok(());
    }

    tracing::info!(project = %subdomain, "waker: container down despite DB=running, recreating");

    let _ = state.docker.remove_container(container_id).await;

    let project_clone = {
        let mut p = project.clone();
        p.container_id = None;
        p.mapped_port = None;
        p
    };

    let extra_env = crate::routes::manage::read_local_project_env(&subdomain);
    let config = litebin_common::types::RunServiceConfig::from_project(&project_clone, extra_env);
    let (new_container_id, new_mapped_port) = match state.docker.run_service_container(&config).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, project = %subdomain, "waker: failed to recreate container");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to recreate container: {e}"),
            )
                .into_response());
        }
    };

    crate::routes::manage::write_local_env_snapshot(&subdomain);

    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE projects SET status = 'running', container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&new_container_id)
    .bind(new_mapped_port as i64)
    .bind(now)
    .bind(now)
    .bind(&subdomain)
    .execute(&state.db)
    .await;

    Ok(())
}

/// Look up a project by alias route. Handles both:
/// - "{alias}.{project_id}" (project-scoped, e.g. "api2.test")
/// - "{alias}" (domain-level, e.g. "api2")
async fn resolve_alias_project(db: &sqlx::SqlitePool, rest: &str) -> Result<Option<crate::db::models::Project>, ()> {
    // Case A: "{alias}.{project_id}" — project-scoped alias
    if let Some((_alias, pid)) = rest.rsplit_once('.') {
        let route_exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM project_routes WHERE project_id = ? AND route_type = 'alias' AND subdomain = ?"
        )
        .bind(pid)
        .bind(_alias)
        .fetch_one(db)
        .await
        .unwrap_or(0);

        if route_exists > 0 {
            return sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
                .bind(pid)
                .fetch_optional(db)
                .await
                .map_err(|_| ());
        }
    }

    // Case B: "{alias}" — domain-level alias
    let alias_pid: Option<String> = sqlx::query_scalar(
        "SELECT project_id FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1"
    )
    .bind(rest)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    if let Some(pid) = alias_pid {
        return sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
            .bind(&pid)
            .fetch_optional(db)
            .await
            .map_err(|_| ());
    }

    Ok(None)
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

    // Fast path: already running with a port — just resync Caddy and return loading page
    // For multi-service projects, check if any service container is running
    let is_multi = project.service_count.unwrap_or(1) > 1;
    if project.status == "running" && project.mapped_port.is_some() && !is_multi {
        if !is_remote {
            if let Some(ref container_id) = project.container_id {
                let actually_running = state
                    .docker
                    .is_container_running(container_id)
                    .await
                    .unwrap_or(true);

                if !actually_running {
                    tracing::info!(project = %project_id, "waker: container down despite DB=running");
                } else {
                    // Port may have drifted (e.g. Docker daemon restarted) — verify and fix
                    if let Ok(actual_port) = state.docker.inspect_mapped_port(container_id).await {
                        let db_port = project.mapped_port.unwrap() as u16;
                        if actual_port != db_port {
                            let now = chrono::Utc::now().timestamp();
                            let _ = sqlx::query(
                                "UPDATE projects SET mapped_port = ?, updated_at = ? WHERE id = ?",
                            )
                            .bind(actual_port as i64)
                            .bind(now)
                            .bind(&project_id)
                            .execute(&state.db)
                            .await;
                            tracing::info!(project = %project_id, old = %db_port, new = %actual_port, "waker: port drifted, updated DB");
                        }
                    }
                    let _ = state.route_sync_tx.send(());
                    return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
                }
            } else {
                let _ = state.route_sync_tx.send(());
                return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
            }
        } else {
            let _ = state.route_sync_tx.send(());
            return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
        }
    } else if project.status == "running" && is_multi {
        // If an operation (deploy, recreate, wake) is in progress, show loading page
        let lock_held = state.project_locks.get(&project_id)
            .map(|s| s.available_permits() == 0)
            .unwrap_or(false);
        if lock_held {
            return if wants_json { starting_json_response() } else { loading_page_html(&project_id).into_response() };
        }

        // Multi-service running: health-check all services (throttled) and proxy to container.
        // This path is hit because multi-service projects always route through the orchestrator.
        let should_check = state
            .multi_svc_health_check
            .get(&project_id)
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(5))
            .unwrap_or(true);

        let mut public_service_up = true;

        if should_check && !is_remote {
            state.multi_svc_health_check.insert(project_id.clone(), std::time::Instant::now());

            let services: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT service_name, container_id FROM project_services WHERE project_id = ? AND status = 'running' AND container_id IS NOT NULL",
            )
            .bind(&project_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let mut stopped_services = Vec::new();
            for (service_name, container_id) in &services {
                if let Some(cid) = container_id {
                    if !state.docker.is_container_running(cid).await.unwrap_or(false) {
                        stopped_services.push(service_name.clone());
                    }
                }
            }

            if !stopped_services.is_empty() {
                tracing::info!(project = %project_id, stopped = ?stopped_services, "waker: multi-service has crashed services");

                let now = chrono::Utc::now().timestamp();
                for service_name in &stopped_services {
                    let _ = sqlx::query(
                        "UPDATE project_services SET status = 'stopped' WHERE project_id = ? AND service_name = ?",
                    )
                    .bind(&project_id)
                    .bind(service_name)
                    .execute(&state.db)
                    .await;
                }

                // Check if the public service is among the crashed ones
                let public_down: bool = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM project_services WHERE project_id = ? AND is_public = 1 AND status = 'stopped'",
                )
                .bind(&project_id)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0) > 0;

                if public_down {
                    // Public service is down — fall through to wake lock (loading page)
                    public_service_up = false;
                    let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                        .bind(now)
                        .bind(&project_id)
                        .execute(&state.db)
                        .await;
                    tracing::info!(project = %project_id, "waker: public service down, marking stopped");
                } else {
                    // Non-public services down but public service up — silently recover in background
                    let _ = sqlx::query("UPDATE projects SET status = 'degraded', updated_at = ? WHERE id = ? AND status != 'degraded'")
                        .bind(now)
                        .bind(&project_id)
                        .execute(&state.db)
                        .await;
                    let _ = state.route_sync_tx.send(());

                    // Spawn background recovery (start_multi_service is idempotent — skips running services)
                    let state_clone = state.clone();
                    let project_clone = project.clone();
                    let project_id_bg = project_id.clone();
                    tokio::spawn(async move {
                        let _permit = try_acquire_project_lock(&state_clone, &project_id_bg);
                        if _permit.is_none() {
                            tracing::info!(project = %project_id_bg, "waker: skipping recovery, another operation in progress");
                            return;
                        }
                        tracing::info!(project = %project_id_bg, "waker: background recovery of degraded services");
                        match start_multi_service(&state_clone, &project_clone).await {
                            Ok(_) => {
                                let _ = state_clone.route_sync_tx.send(());
                                tracing::info!(project = %project_id_bg, "waker: background recovery succeeded");
                            }
                            Err(resp) => tracing::warn!(project = %project_id_bg, status = %resp.status(), "waker: background recovery failed"),
                        }
                    });
                }
            }
        }

        if public_service_up {
            // Public service is healthy — proxy the request to the container
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
                // Retry proxy up to 3 times with short delay — handles Docker DNS propagation
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

            // No public service found with status='running' in DB — check if it's actually running in docker
            // (DB may be stale, e.g. status is "error" or "stopped" but container was restarted externally)
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
                        // Container is actually running — sync DB and proxy
                        tracing::info!(project = %project_id, service = %svc_name, "waker: public service running but DB stale, syncing status");
                        let now = chrono::Utc::now().timestamp();
                        let _ = sqlx::query("UPDATE project_services SET status = 'running' WHERE project_id = ? AND service_name = ?")
                            .bind(&project_id)
                            .bind(&svc_name)
                            .execute(&state.db)
                            .await;
                        let _ = sqlx::query("UPDATE projects SET status = 'running', updated_at = ? WHERE id = ?")
                            .bind(now)
                            .bind(&project_id)
                            .execute(&state.db)
                            .await;

                        let container_name = litebin_common::types::container_name(&project_id, &svc_name, None);
                        let upstream = format!("{}:{}", container_name, port.unwrap_or(80) as u16);
                        let resp = proxy_request(&state.proxy_client, method.clone(), &upstream, uri.path_and_query().map(|pq| pq.as_str()), headers, body.clone()).await;
                        return resp;
                    }
                }
            }

            // Public service truly not running — fall through to wake lock
            tracing::warn!(project = %project_id, "waker: multi-service public service not running, falling through");
        }
        // If public service is down, fall through to wake lock below (loading page + start all)
    } else if project.status == "degraded" {
        // Degraded: some services stopped, some running. Proxy to public service if it's up,
        // recover remaining in background.
        tracing::info!(project = %project_id, "waker: degraded project, starting remaining services");

        // Proxy to public service if it's running — don't fall through to wake lock
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

            // Spawn background recovery (idempotent — skips already-running services)
            let state_clone = state.clone();
            let project_clone = project.clone();
            let project_id_bg = project_id.clone();
            tokio::spawn(async move {
                let _permit = try_acquire_project_lock(&state_clone, &project_id_bg);
                if _permit.is_none() {
                    tracing::info!(project = %project_id_bg, "waker: skipping recovery, another operation in progress");
                    return;
                }
                tracing::info!(project = %project_id_bg, "waker: background recovery of degraded services");
                match start_multi_service(&state_clone, &project_clone).await {
                    Ok(_) => {
                        let _ = state_clone.route_sync_tx.send(());
                        tracing::info!(project = %project_id_bg, "waker: background recovery succeeded");
                    }
                    Err(resp) => tracing::warn!(project = %project_id_bg, status = %resp.status(), "waker: background recovery failed"),
                }
            });

            // Retry proxy up to 3 times with short delay — handles Docker DNS propagation
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
        // Public service is down — fall through to wake lock + start_multi_service below
    } else if project.status == "running" && project.mapped_port.is_none() {
        tracing::info!(project = %project_id, "waker: running but mapped_port is null, recreating");
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

    let is_stopped = project.status == "stopped" || project.status == "degraded";
    let state_clone = state.clone();
    let project_clone = project.clone();
    let project_id_bg = project_id.clone();

    tracing::info!(project = %project_id, host = %host, "waker: spawning background wake");

    tokio::spawn(async move {
        let wake_fut = if is_multi {
            start_multi_service(&state_clone, &project_clone).boxed()
        } else if is_stopped {
            start_stopped_container(&state_clone, &project_clone).boxed()
        } else {
            restart_crashed_container(&state_clone, &project_clone).boxed()
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

    // Let dashboard, poke, and bare domain requests pass through to the router
    if host_without_port == config.domain
        || host_without_port == dashboard_host
        || host_without_port == poke_host
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
