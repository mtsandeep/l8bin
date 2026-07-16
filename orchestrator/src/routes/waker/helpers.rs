use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use litebin_common::proxy::is_hop_by_hop;
use litebin_common::types::{Node, ProjectStatus};

use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::status::{self, ProjectUpdateFields};
use crate::AppState;

/// Try to acquire the per-project lock. Returns None if another operation is in progress.
pub(super) fn try_acquire_project_lock(state: &AppState, project_id: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let semaphore: Arc<Semaphore> = state.project_locks
        .entry(project_id.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    semaphore.clone().try_acquire_owned().ok()
}

/// Reverse-proxy a request to a container on the Docker network.
/// Streams the response back to the client.
pub(super) async fn proxy_request(
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

/// Recreate a container on a remote agent (no image pull).
pub(super) async fn remote_recreate(
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
            "volumes": project.volumes.as_ref().and_then(|v| {
                match serde_json::from_str::<Vec<litebin_common::types::VolumeMount>>(v) {
                    Ok(mounts) => Some(mounts),
                    Err(e) => {
                        tracing::error!(project = %project.id, error = %e, "waker: failed to parse volumes JSON");
                        None
                    }
                }
            }),
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
    if let Err(e) = status::transition(&state.db, &project.id, ProjectStatus::Running, &ProjectUpdateFields {
        container_id: Some(Some(new_container_id.clone())),
        mapped_port: Some(mapped_port.map(|p| p as i64)),
        last_active_at: Some(now),
        ..Default::default()
    }, None).await {
        tracing::warn!(project_id = %project.id, error = %e, "waker: failed to transition to Running");
    }

    Ok(())
}

/// Start only the stopped services of a project (targeted recovery).
/// Used by the waker when non-public services are down but the public service is up.
pub(super) async fn start_stopped_services(state: &AppState, project: &crate::db::models::Project) {
    let stopped: Vec<String> = match sqlx::query_scalar(
        "SELECT service_name FROM project_services WHERE project_id = ? AND status = 'stopped' AND is_oneshot = 0"
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project_id = %project.id, error = %e, "waker: failed to fetch stopped services");
            return;
        }
    };

    if stopped.is_empty() {
        return;
    }

    let filter: std::collections::HashSet<String> = stopped.into_iter().collect();
    match crate::routes::manage::start_services(state, project, crate::routes::manage::StartServicesOpts {
        force_recreate: false,
        pull_images: true,
        force_pull: false,
        services: Some(filter),
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await {
        Ok(_) => {
            status::derive_and_set_project_status(&state.db, &project.id).await;
            let _ = state.route_sync_tx.send(());
            tracing::info!(project = %project.id, "waker: background recovery succeeded");
        }
        Err((s, e)) => tracing::warn!(project = %project.id, status = %s, error = %e, "waker: background recovery failed"),
    }
}

/// Handle detected down services: transition project status and spawn recovery.
/// If the public service is down, mark project "stopped" (fall through to wake lock).
/// If only non-public services are down, mark "degraded" and recover in background.
pub(super) async fn handle_down_services(
    state: &AppState,
    project: &crate::db::models::Project,
    project_id: &str,
    public_service_up: &mut bool,
) {
    // After marking crashed services as stopped, check if public is among them
    let public_down = match sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM project_services WHERE project_id = ? AND is_public = 1 AND status = 'stopped'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(count) => count > 0,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "waker: failed to check public service status, assuming up");
            false
        }
    };

    if public_down {
        *public_service_up = false;
        if let Err(e) = status::transition(&state.db, project_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None).await {
            tracing::warn!(project_id = %project_id, error = %e, "waker: failed to transition to Stopped");
        }
    } else {
        if let Err(e) = status::transition(&state.db, project_id, ProjectStatus::Degraded, &ProjectUpdateFields::default(), None).await {
            tracing::warn!(project_id = %project_id, error = %e, "waker: failed to transition to Degraded");
        }
        let _ = state.route_sync_tx.send(());
        let state_clone = state.clone();
        let project_clone = project.clone();
        let project_id_bg = project_id.to_string();
        tokio::spawn(async move {
            let _permit = try_acquire_project_lock(&state_clone, &project_id_bg);
            if _permit.is_none() {
                return;
            }
            start_stopped_services(&state_clone, &project_clone).await;
        });
    }
}

pub(super) async fn start_stopped_container(state: &AppState, project: &crate::db::models::Project) -> Result<(), Response> {
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
            let result: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(project = %subdomain, error = %e, "waker: failed to parse agent start response");
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to parse agent response").into_response());
                }
            };
            let mapped_port = result["mapped_port"].as_u64().map(|p| p as u16);

            let now = chrono::Utc::now().timestamp();
            if let Err(e) = status::transition(&state.db, &subdomain, ProjectStatus::Running, &ProjectUpdateFields {
                mapped_port: Some(mapped_port.map(|p| p as i64)),
                last_active_at: Some(now),
                ..Default::default()
            }, None).await {
                tracing::warn!(project_id = %subdomain, error = %e, "waker: failed to transition to Running after start");
            }
            return Ok(());
        }

        // Start failed — container may have been pruned. Fall back to recreate.
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(project = %subdomain, body = %body, "waker: agent start failed, trying recreate");
        return remote_recreate(state, project, &client, &base_url).await;
    }

    // Local: use unified start_services
    crate::routes::manage::start_services(state, project, crate::routes::manage::StartServicesOpts {
        force_recreate: false,
        pull_images: true,
        force_pull: false,
        services: None,
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await.map_err(|(s, e)| (s, e).into_response())
}

pub(super) async fn restart_crashed_container(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<(), Response> {
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
        tracing::info!(project = %project.id, "waker: remote container down despite DB=running, recreating");
        return remote_recreate(state, project, &client, &base_url).await;
    }

    // Local: use unified start_services (force_recreate since container is dead)
    crate::routes::manage::start_services(state, project, crate::routes::manage::StartServicesOpts {
        force_recreate: true,
        pull_images: false,
        force_pull: false,
        services: None,
        connect_orchestrator: true,
        rollback_on_failure: false,
    }).await.map_err(|(s, e)| (s, e).into_response())
}

/// Look up a project by alias route. Handles both:
/// - "{alias}.{project_id}" (project-scoped, e.g. "api2.test")
/// - "{alias}" (domain-level, e.g. "api2")
pub(super) async fn resolve_alias_project(db: &sqlx::SqlitePool, rest: &str) -> Result<Option<crate::db::models::Project>, ()> {
    // Case A: "{alias}.{project_id}" — project-scoped alias
    if let Some((_alias, pid)) = rest.rsplit_once('.') {
        let route_exists = match sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM project_routes WHERE project_id = ? AND route_type = 'alias' AND subdomain = ?"
        )
        .bind(pid)
        .bind(_alias)
        .fetch_one(db)
        .await
        {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!(project_id = %pid, alias = %_alias, error = %e, "waker: failed to check alias route existence");
                return Err(());
            }
        };

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
