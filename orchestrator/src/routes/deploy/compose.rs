use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use axum::extract::Multipart;
use axum_login::AuthSession;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::auth::backend::PasswordBackend;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

/// POST /deploy/compose — Deploy a multi-service project via compose file.
///
/// Accepts multipart form data with:
/// - `project_id` (text field)
/// - `name` (optional text field)
/// - `description` (optional text field)
/// - `node_id` (optional text field)
/// - `auto_stop_enabled` (optional text field, "true"/"false")
/// - `auto_stop_timeout_mins` (optional text field)
/// - `auto_start_enabled` (optional text field, "true"/"false")
/// - `custom_domain` (optional text field)
/// - `compose` (file field — the docker-compose.yaml content)
pub async fn deploy_compose(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Parse multipart fields
    let mut project_id = None;
    let mut name = None;
    let mut description = None;
    let mut node_id = None;
    let mut auto_stop_enabled = None;
    let mut auto_stop_timeout_mins = None;
    let mut auto_start_enabled = None;
    let mut custom_domain = None;
    let mut compose_content = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = match field.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        match field_name.as_str() {
            "project_id" => {
                project_id = field.text().await.ok();
            }
            "name" => {
                name = field.text().await.ok();
            }
            "description" => {
                description = field.text().await.ok();
            }
            "node_id" => {
                node_id = field.text().await.ok();
            }
            "auto_stop_enabled" => {
                auto_stop_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "auto_stop_timeout_mins" => {
                auto_stop_timeout_mins = field.text().await.ok().and_then(|v| v.parse::<i64>().ok());
            }
            "auto_start_enabled" => {
                auto_start_enabled = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "custom_domain" => {
                custom_domain = field.text().await.ok();
            }
            "compose" => {
                compose_content = field.bytes().await.ok();
            }
            _ => {
                tracing::debug!(field = %field_name, "ignoring unknown multipart field");
            }
        }
    }

    let project_id = match project_id {
        Some(id) if !id.is_empty() => id,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "project_id is required"})),
        ).into_response(),
    };

    let compose_bytes = match compose_content {
        Some(b) if !b.is_empty() => b,
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file is required"})),
        ).into_response(),
    };

    let compose_yaml = match String::from_utf8(compose_bytes.to_vec()) {
        Ok(s) => s,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("compose file is not valid UTF-8: {e}")})),
        ).into_response(),
    };

    // Authenticate
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => {
            match crate::auth::extract_deploy_token(&state, &headers, &project_id).await {
                Some(uid) => uid,
                None => return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
                ).into_response(),
            }
        }
    };

    // Basic validation
    if project_id == state.config.dashboard_subdomain || project_id == state.config.poke_subdomain {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "This ID is reserved"})),
        ).into_response();
    }
    if !crate::validation::is_valid_project_id(&project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Project ID must be 1-63 lowercase letters, digits, or hyphens"})),
        ).into_response();
    }

    // Parse compose file
    let compose = match compose_bollard::ComposeParser::parse(&compose_yaml) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid compose YAML: {e}")})),
        ).into_response(),
    };

    if compose.services.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "compose file has no services"})),
        ).into_response();
    }

    // 4 validation checks
    // 1. Ghost deps
    let ghosts = compose.validate_ghost_deps();
    if !ghosts.is_empty() {
        let msg = ghosts.iter()
            .map(|(svc, dep)| format!("service '{svc}' depends on unknown service '{dep}'"))
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid dependencies: {msg}")})),
        ).into_response();
    }

    // 2. Cycles
    if let Some(cycle) = compose.detect_cycles() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("dependency cycle detected: {}", cycle.join(" -> "))})),
        ).into_response();
    }

    // 3. Topological sort (also validates DAG)
    let start_order = match compose.topological_sort() {
        Ok(order) => order,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid service graph: {e}")})),
        ).into_response(),
    };

    // 4. Public service detection (warning only if none found)
    let public_service = match compose.detect_public_service() {
        Ok(s) => s,
        Err(e) => return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("public service conflict: {e}")})),
        ).into_response(),
    };

    let now = chrono::Utc::now().timestamp();
    let auto_stop = auto_stop_enabled.unwrap_or(true);
    let auto_stop_mins = auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start = auto_start_enabled.unwrap_or(true);

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        public = ?public_service,
        "compose deploy request received"
    );

    // Acquire deploy lock
    let semaphore = state
        .project_locks
        .entry(project_id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone();
    let _permit = semaphore.acquire().await.unwrap();

    // Ensure project directory exists and write compose.yaml to disk
    crate::routes::manage::ensure_project_dir_and_env(&project_id);
    let compose_path = std::path::PathBuf::from("projects").join(&project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &compose_yaml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to write compose.yaml: {e}")})),
        ).into_response();
    }

    // Determine the public service's port for the projects row
    let public_svc_name = public_service.as_deref().unwrap_or(&start_order[0]);
    let public_svc = &compose.services[public_svc_name];
    let public_port: Option<i64> = public_svc.exposed_ports().first().map(|(p, _)| *p as i64);
    let public_image = public_svc.image.clone().unwrap_or_default();

    // Build service_count and service_summary
    let service_count = compose.services.len() as i64;
    let service_summary = start_order.join(":");

    // Upsert project row
    let result = sqlx::query(
        r#"
        INSERT INTO projects (id, user_id, name, description, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, custom_domain, service_count, service_summary, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?, ?, ?)
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
            custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE COALESCE(projects.custom_domain, excluded.custom_domain) END,
            service_count = excluded.service_count,
            service_summary = excluded.service_summary,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&project_id)
    .bind(&user_id)
    .bind(&name)
    .bind(&description)
    .bind(&public_image)
    .bind(public_port)
    .bind(auto_stop)
    .bind(auto_stop_mins)
    .bind(auto_start)
    .bind(&custom_domain)
    .bind(service_count)
    .bind(&service_summary)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        let msg = e.to_string();
        let status = if msg.contains("UNIQUE constraint") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return (
            status,
            Json(json!({"error": if msg.contains("UNIQUE constraint") { format!("project '{}' already exists", project_id) } else { format!("database error: {msg}") } })),
        ).into_response();
    }

    // Read project back from DB
    let project = match sqlx::query_as::<_, crate::db::models::Project>(
        "SELECT * FROM projects WHERE id = ?"
    )
    .bind(&project_id)
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

    // Seed project_services rows for each service in the compose file
    for svc_name in &start_order {
        let svc = &compose.services[svc_name];
        let image = svc.image.clone().unwrap_or_default();
        let port: Option<i64> = svc.ports.as_ref()
            .and_then(|p| p.first())
            .and_then(|p| p.split(':').last()?.parse().ok())
            .map(|p: u16| p as i64);
        let is_public = public_service.as_deref() == Some(svc_name.as_str());
        let depends_on = svc.depends_on.as_ref()
            .and_then(|d| serde_json::to_string(d).ok());
        let memory_limit_mb: Option<i64> = svc.memory_bytes()
            .map(|bytes| (bytes / (1024 * 1024)) as i64);
        let cpu_limit: Option<f64> = svc.cpus.as_ref()
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())));

        let _ = sqlx::query(
            "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, is_public, depends_on, memory_limit_mb, cpu_limit, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'deploying')"
        )
        .bind(&project_id)
        .bind(svc_name)
        .bind(&image)
        .bind(port)
        .bind(is_public)
        .bind(&depends_on)
        .bind(memory_limit_mb)
        .bind(cpu_limit)
        .execute(&state.db)
        .await;
    }

    let target_node_id = match nodes::selector::select_node(&state.db, &project, node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now).bind(&project_id).execute(&state.db).await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("{:?}", e)})),
            ).into_response();
        }
    };

    // Local vs remote deploy path
    if target_node_id != "local" {
        // Remote multi-service deploy via agent batch-run
        let node = match crate::routes::manage::get_node_from_db(&state.db, &target_node_id).await {
            Ok(n) => n,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now).bind(&project_id).execute(&state.db).await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("{:?}", e)})),
                ).into_response();
            }
        };

        let client = match nodes::client::get_node_client(&state.node_clients, &target_node_id) {
            Ok(c) => c,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now).bind(&project_id).execute(&state.db).await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client unavailable: {:?}", e)})),
                ).into_response();
            }
        };

        let base_url = agent_base_url(&state.config, &node);

        let batch_resp = match client
            .post(&format!("{}/containers/batch-run", base_url))
            .json(&json!({
                "project_id": project_id,
                "compose_yaml": compose_yaml,
                "service_order": start_order,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "remote batch-run request failed");
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now).bind(&project_id).execute(&state.db).await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("agent unreachable: {e}")})),
                ).into_response();
            }
        };

        if !batch_resp.status().is_success() {
            let status_code = batch_resp.status();
            let body = batch_resp.text().await.unwrap_or_default();
            tracing::error!(status = %status_code, body = %body, "remote batch-run failed");
            let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                .bind(now).bind(&project_id).execute(&state.db).await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("remote batch-run failed: {body}")})),
            ).into_response();
        }

        let batch_result: serde_json::Value = match batch_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now).bind(&project_id).execute(&state.db).await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to parse batch-run response: {e}")})),
                ).into_response();
            }
        };

        // Update project_services with container IDs and ports from agent response
        if let Some(services) = batch_result["services"].as_array() {
            for svc in services {
                let svc_name = svc["service_name"].as_str().unwrap_or("");
                let container_id = svc["container_id"].as_str();
                let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);

                let status = if container_id.is_some() { "running" } else { "error" };
                let _ = sqlx::query(
                    "UPDATE project_services SET container_id = ?, mapped_port = ?, status = ? WHERE project_id = ? AND service_name = ?"
                )
                .bind(container_id)
                .bind(mapped_port)
                .bind(status)
                .bind(&project_id)
                .bind(svc_name)
                .execute(&state.db)
                .await;
            }

            // Set project's denormalized container_id to the public service
            let pub_svc_name = public_service.as_deref().unwrap_or("");
            let public_service = services.iter().find(|s| {
                s["service_name"].as_str() == Some(pub_svc_name)
            });

            if let Some(pub_svc) = public_service {
                let cid = pub_svc["container_id"].as_str().unwrap_or("").to_string();
                let port = pub_svc["mapped_port"].as_u64().map(|p| p as i64);
                let _ = sqlx::query(
                    "UPDATE projects SET container_id = ?, mapped_port = ?, status = 'running', last_active_at = ?, updated_at = ? WHERE id = ?"
                )
                .bind(&cid)
                .bind(port)
                .bind(now)
                .bind(now)
                .bind(&project_id)
                .execute(&state.db)
                .await;
            }
        }

        // Trigger route sync
        let _ = state.route_sync_tx.send(());

        return (
            StatusCode::OK,
            Json(json!({"status": "deployed", "project_id": project_id})),
        ).into_response();
    }

    // Clean up existing containers from a previous deploy (by name prefix).
    // Handles orphaned containers from failed deploys.
    let prefix = format!("litebin-{}.", project_id);
    if let Ok(all_containers) = state.docker.list_containers_by_prefix(&prefix).await {
        for cid in &all_containers {
            let _ = state.docker.stop_container(cid).await;
            let _ = state.docker.remove_container(cid).await;
        }
    }

    // Pull all images before starting (fail on any pull error)
    let images: Vec<String> = start_order.iter()
        .filter_map(|name| compose.services[name].image.clone())
        .collect();
    let mut pull_errors = Vec::new();
    for image in &images {
        if !image.starts_with("sha256:") {
            if let Err(e) = state.docker.pull_image(image).await {
                pull_errors.push(format!("{}: {}", image, e));
            }
        }
    }
    if !pull_errors.is_empty() {
        let msg = pull_errors.join("; ");
        tracing::error!(errors = %msg, "failed to pull images");
        let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
            .bind(now).bind(&project_id).execute(&state.db).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to pull images: {msg}")})),
        ).into_response();
    }

    // Start all services using the unified function.
    // Network setup, Caddy/orchestrator connect, health checks, DB updates,
    // and DNS wait are all handled internally by start_services.
    if let Err((status, msg)) = crate::routes::manage::start_services(
        &state,
        &project,
        crate::routes::manage::StartServicesOpts {
            force_recreate: true,
            pull_images: false, // already pulled above
            services: None,
            connect_orchestrator: true,
            rollback_on_failure: true,
        },
    ).await {
        return (status, Json(json!({"error": msg}))).into_response();
    }

    // Full route sync after deploy
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let route_entries = match crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve routes after compose deploy");
            Vec::new()
        }
    };
    let _ = state
        .router
        .read()
        .await
        .sync_routes(&route_entries, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await;

    let url = format!("https://{}.{}", project_id, state.config.domain);

    tracing::info!(
        project_id = %project_id,
        services = start_order.len(),
        url = %url,
        "compose deploy complete"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "running",
            "project_id": project_id,
            "url": url,
            "custom_domain": custom_domain,
        })),
    )
        .into_response()
}
