use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use axum_login::AuthSession;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::auth::backend::PasswordBackend;
use litebin_common::types::Node;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

#[derive(Deserialize)]
pub struct DeployRequest {
    pub project_id: String,
    pub image: String,
    pub port: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_id: Option<String>, // optional override
    pub auto_stop_enabled: Option<bool>,
    pub auto_stop_timeout_mins: Option<i64>,
    pub auto_start_enabled: Option<bool>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub custom_domain: Option<String>,
}

pub async fn deploy(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DeployRequest>,
) -> impl IntoResponse {
    // Auth: session first, then deploy token fallback
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            // Try deploy token from Authorization: Bearer <token>
            match crate::auth::extract_deploy_token(&state, &headers, &payload.project_id).await {
                Some(uid) => uid,
                None => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
                    )
                        .into_response();
                }
            }
        }
    };

    let now = chrono::Utc::now().timestamp();

    let auto_stop_enabled = payload.auto_stop_enabled.unwrap_or(true);
    let auto_stop_timeout_mins = payload.auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start_enabled = payload.auto_start_enabled.unwrap_or(true);

    tracing::info!(
        project_id = %payload.project_id,
        image = %payload.image,
        port = %payload.port,
        "deploy request received"
    );

    // Reserve the dashboard subdomain
    if payload.project_id == state.config.dashboard_subdomain {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "This ID is reserved"})),
        ).into_response();
    }

    // Reserve the poke subdomain
    if payload.project_id == state.config.poke_subdomain {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "This ID is reserved"})),
        ).into_response();
    }

    // Validate project ID (DNS-safe label)
    if !crate::validation::is_valid_project_id(&payload.project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens (no leading/trailing hyphens)"})),
        ).into_response();
    }

    // 1. Acquire deploy lock for this project_id (serializes concurrent deploys)
    let semaphore = state
        .deploy_locks
        .entry(payload.project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // 1b. Capture old image before upsert (for cleanup after deploy)
    let old_image = sqlx::query_scalar::<_, String>("SELECT image FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    // 2. Upsert project in DB with status='deploying'
    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            user_id = excluded.user_id,
            image = excluded.image,
            internal_port = excluded.internal_port,
            status = 'deploying',
            name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
            description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
            auto_stop_enabled = excluded.auto_stop_enabled,
            auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
            auto_start_enabled = excluded.auto_start_enabled,
            cmd = CASE WHEN excluded.cmd IS NOT NULL THEN excluded.cmd ELSE projects.cmd END,
            memory_limit_mb = CASE WHEN excluded.memory_limit_mb IS NOT NULL THEN excluded.memory_limit_mb ELSE projects.memory_limit_mb END,
            cpu_limit = CASE WHEN excluded.cpu_limit IS NOT NULL THEN excluded.cpu_limit ELSE projects.cpu_limit END,
            custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE projects.custom_domain END,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&payload.project_id)
    .bind(&user_id)
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(&payload.image)
    .bind(payload.port)
    .bind(auto_stop_enabled)
    .bind(auto_stop_timeout_mins)
    .bind(auto_start_enabled)
    .bind(&payload.cmd)
    .bind(payload.memory_limit_mb)
    .bind(payload.cpu_limit)
    .bind(&payload.custom_domain)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("database error: {e}")})),
        )
            .into_response();
    }

    // 3. Read project back from DB (so cmd reflects stored value, not just payload)
    let project = match sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?"
    )
    .bind(&payload.project_id)
    .fetch_one(&state.db)
    .await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("database error: {e}")})),
            ).into_response();
        }
    };

    // 4. Select target node
    let node_id = match nodes::selector::select_node(&state.db, &project, payload.node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            let _ = sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // 5. Branch: local vs remote node
    let (container_id, mapped_port) = if node_id == "local" {
        // --- Local path ---

        // Remove any existing container for this project
        if let Err(e) = state.docker.remove_by_name(&payload.project_id).await {
            tracing::warn!(error = %e, "failed to remove old container (may not exist)");
        }

        // Pull the new image (skip if it's a local image ID, i.e. already uploaded)
        if !payload.image.starts_with("sha256:") {
            if let Err(e) = state
                .docker
                .pull_image(&payload.image)
                .await
            {
                tracing::error!(error = %e, "failed to pull image");
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to pull image: {e}")})),
            )
                .into_response();
            }
        }

        // Start the container
        match state.docker.run_container(&project, Vec::new(), None).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!(error = %e, "failed to start container");
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to start container: {e}")})),
                )
                    .into_response();
            }
        }
    } else {
        // --- Remote node path ---

        // Get the node record for host/port
        let node = match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) => n,
            Ok(None) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node '{}' not found", node_id)})),
                )
                    .into_response();
            }
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("database error: {e}")})),
                )
                    .into_response();
            }
        };

        // Get client from pool
        let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
            Ok(c) => c,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client not available: {e}")})),
                )
                    .into_response();
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        // Run container on agent
        let run_resp = match client
            .post(&format!("{}/containers/run", base_url))
            .json(&json!({
                "image": payload.image,
                "internal_port": payload.port,
                "project_id": payload.project_id,
                "cmd": project.cmd,
                "memory_limit_mb": project.memory_limit_mb,
                "cpu_limit": project.cpu_limit,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, node_id = %node_id, "failed to run container on agent");
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("agent unreachable during container run: {e}")})),
                )
                    .into_response();
            }
        };

        if !run_resp.status().is_success() {
            let status_code = run_resp.status();
            let body = run_resp.text().await.unwrap_or_default();
            tracing::error!(node_id = %node_id, status = %status_code, "container run failed");
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&payload.project_id)
                .execute(&state.db)
                .await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("container run failed: {body}")})),
            )
                .into_response();
        }

        let run_json: serde_json::Value = match run_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("invalid container run response: {e}")})),
                )
                    .into_response();
            }
        };

        let container_id = match run_json["container_id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "missing container_id in run response"})),
                )
                    .into_response();
            }
        };

        let mapped_port = match run_json["mapped_port"].as_u64() {
            Some(p) => p as u16,
            None => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&payload.project_id)
                    .execute(&state.db)
                    .await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "missing mapped_port in run response"})),
                )
                    .into_response();
            }
        };

        (container_id, mapped_port)
    };

    // 6. Update DB with container info and node_id
    if let Err(e) = sqlx::query(
        r#"
        UPDATE projects
        SET container_id = ?, mapped_port = ?, node_id = ?, status = 'running',
            last_active_at = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&container_id)
    .bind(mapped_port as i64)
    .bind(&node_id)
    .bind(now)
    .bind(now)
    .bind(&payload.project_id)
    .execute(&state.db)
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("database error: {e}")})),
        )
            .into_response();
    }

    // 7. Sync Caddy routes
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let route_entries = match crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve routes");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to resolve routes: {e}")})),
            )
                .into_response();
        }
    };
    if let Err(e) = state
        .router
        .read()
        .await
        .sync_routes(&route_entries, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await
    {
        tracing::error!(error = %e, "failed to sync routes — rolling back container");

        // Roll back: stop the container and reset DB status
        if node_id == "local" {
            let _ = state.docker.stop_container(&container_id).await;
            let _ = state.docker.remove_container(&container_id).await;
        } else {
            let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
                .bind(&node_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
            if let Some(node) = node {
                if let Ok(client) = nodes::client::get_node_client(&state.node_clients, &node_id) {
                    let url = agent_base_url(&state.config, &node);
                    let _ = client
                        .post(format!("{}/containers/stop", url))
                        .json(&json!({"container_id": &container_id}))
                        .send()
                        .await;
                }
            }
        }
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&payload.project_id)
            .execute(&state.db)
            .await;

        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to configure routing: {e}")})),
        )
            .into_response();
    }

    let url = format!("https://{}.{}", payload.project_id, state.config.domain);

    // 8. Clean up old image if it changed
    if let Some(ref old) = old_image {
        if old != &payload.image {
            crate::routes::manage::cleanup_unused_image(
                &state,
                Some(&node_id),
                old,
            )
            .await;
        }
    }

    tracing::info!(
        project_id = %payload.project_id,
        container_id = %container_id,
        mapped_port = %mapped_port,
        node_id = %node_id,
        url = %url,
        "deploy complete"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "running",
            "project_id": payload.project_id,
            "url": url,
            "custom_domain": payload.custom_domain,
            "mapped_port": mapped_port
        })),
    )
        .into_response()
}


#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use dashmap::DashMap;

    #[tokio::test]
    async fn prop_deploy_lock_serializes_concurrent_ops() {
        let deploy_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());
        let project_id = "test-project";

        // Create semaphore for project
        let semaphore = deploy_locks
            .entry(project_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let concurrent_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = vec![];
        for _ in 0..5 {
            let sem = semaphore.clone();
            let counter = counter.clone();
            let concurrent = concurrent_count.clone();
            let max_c = max_concurrent.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let c = concurrent.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_c.fetch_max(c, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                concurrent.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 5 tasks completed
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 5);
        // Never more than 1 concurrent holder
        assert_eq!(max_concurrent.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
