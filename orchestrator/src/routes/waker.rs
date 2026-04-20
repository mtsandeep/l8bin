use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use futures_util::FutureExt;
use serde_json::json;
use std::sync::Arc;

use litebin_common::types::Node;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

fn loading_page_html(subdomain: &str) -> Html<String> {
    Html(format!(
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
        subdomain, subdomain
    ))
}

fn error_page_html() -> Html<String> {
    Html(String::from(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta http-equiv="refresh" content="30">
    <title>Offline</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .msg {{ text-align: center; }}
        h2 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }}
        p {{ color: #64748b; margin: 0; font-size: 0.875rem; }}
    </style>
</head>
<body>
    <div class="msg">
        <h2>Failed to start the website</h2>
        <p>Retrying in 30 seconds...</p>
    </div>
</body>
</html>"#,
    ))
}

fn not_found_page_html() -> Html<String> {
    Html(String::from(
        r#"<!DOCTYPE html>
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
</html>"#,
    ))
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
        // Local: check if env changed to decide start vs recreate
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
        let (new_container_id, new_mapped_port) = match state.docker.run_container(&project_clone, extra_env, None).await {
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
    let (new_container_id, new_mapped_port) = match state.docker.run_container(&project_clone, extra_env, None).await {
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

/// Catch-all handler. Caddy proxies here when no project-specific route matches.
/// Uses single-flight dedup — the first request spawns the wake in the background;
/// all concurrent requests get the loading page immediately.
pub async fn wake(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

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
            Ok(None) => None,
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
    if project.status == "running" && project.mapped_port.is_some() {
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
                    return loading_page_html(&project_id).into_response();
                }
            } else {
                let _ = state.route_sync_tx.send(());
                return loading_page_html(&project_id).into_response();
            }
        } else {
            let _ = state.route_sync_tx.send(());
            return loading_page_html(&project_id).into_response();
        }
    } else if project.status == "running" && project.mapped_port.is_none() {
        tracing::info!(project = %project_id, "waker: running but mapped_port is null, recreating");
    }

    if !project.auto_start_enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(format!(
                r#"<!DOCTYPE html>
<html>
<head>
    <title>Offline</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .msg {{ text-align: center; }}
        h2 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }}
        p {{ color: #64748b; margin: 0; font-size: 0.875rem; }}
    </style>
</head>
<body>
    <div class="msg">
        <h2>This website is currently offline</h2>
        <p>Auto-start is disabled!</p>
    </div>
</body>
</html>"#,
            )),
        )
            .into_response();
    }

    // Single-flight dedup: first caller spawns background wake, all get loading page immediately.
    // On failure, the lock stays with completed=true+success=false so subsequent refreshes
    // see the error page instead of retrying infinitely. Auto-cleared after 60s.
    let guard = Arc::new(crate::WakeGuard {
        notify: tokio::sync::Notify::new(),
        success: std::sync::atomic::AtomicBool::new(false),
        completed: std::sync::atomic::AtomicBool::new(false),
    });

    match state.wake_locks.entry(project_id.clone()) {
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            let guard = entry.insert(guard);

            let is_stopped = project.status == "stopped";
            let state_clone = state.clone();
            let project_clone = project.clone();
            let project_id_bg = project_id.clone();
            let guard_bg = guard.clone();

            tracing::info!(project = %project_id, host = %host, "waker: spawning background wake");

            tokio::spawn(async move {
                let wake_fut = if is_stopped {
                    start_stopped_container(&state_clone, &project_clone).boxed()
                } else {
                    restart_crashed_container(&state_clone, &project_clone).boxed()
                };

                let result = tokio::time::timeout(std::time::Duration::from_secs(60), wake_fut).await;

                let success = matches!(result, Ok(Ok(())));
                guard_bg.success.store(success, std::sync::atomic::Ordering::Release);
                guard_bg.completed.store(true, std::sync::atomic::Ordering::Release);

                if success {
                    let _ = state_clone.route_sync_tx.send(());
                    guard_bg.notify.notify_waiters();
                    state_clone.wake_locks.remove(&project_id_bg);
                } else {
                    if result.is_err() {
                        tracing::error!(project = %project_id_bg, "waker: background wake timed out");
                    } else {
                        tracing::error!(project = %project_id_bg, "waker: background wake failed");
                    }
                    guard_bg.notify.notify_waiters();
                    // Keep the lock so subsequent requests see the failure.
                    // Auto-clear after 60s to allow retry.
                    let locks = state_clone.wake_locks.clone();
                    let pid = project_id_bg.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        locks.remove(&pid);
                    });
                }
            });

            loading_page_html(&project_id).into_response()
        }
        dashmap::mapref::entry::Entry::Occupied(entry) => {
            let guard = entry.get().clone();
            // Check if a previous wake completed with failure
            if guard.completed.load(std::sync::atomic::Ordering::Acquire)
                && !guard.success.load(std::sync::atomic::Ordering::Acquire)
            {
                return error_page_html().into_response();
            }
            tracing::info!(project = %project_id, "waker: wake already in progress");
            loading_page_html(&project_id).into_response()
        }
    }
}
