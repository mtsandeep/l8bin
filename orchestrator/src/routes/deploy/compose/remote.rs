use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::AppState;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use super::form::ComposeForm;
use super::persist::PersistedCompose;
use super::validate::ValidatedCompose;

/// Remote multi-service deploy via the agent's batch-run endpoint.
pub(super) async fn remote_deploy_path(
    state: &AppState,
    form: &ComposeForm,
    v: &ValidatedCompose,
    p: &PersistedCompose,
    now: i64,
) -> axum::response::Response {
    let project_id = &form.project_id;
    let target_node_id = &p.target_node_id;

    let node = match crate::routes::manage::get_node_from_db(&state.db, &target_node_id).await {
        Ok(n) => n,
        Err(e) => {
            if let Err(e) =
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("{:?}", e)}))).into_response();
        }
    };

    let client = match nodes::client::get_node_client(&state.node_clients, &target_node_id) {
        Ok(c) => c,
        Err(e) => {
            if let Err(e) =
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("node client unavailable: {:?}", e)})),
            )
                .into_response();
        }
    };

    let base_url = agent_base_url(&state.config, &node);
    // Read per-service resource overrides and global defaults to send to agent
    let service_resources: std::collections::HashMap<String, serde_json::Value> =
        match sqlx::query_as::<_, (String, Option<i64>, Option<f64>)>(
            "SELECT service_name, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ?",
        )
        .bind(&project_id)
        .fetch_all(&state.db)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to fetch service resource overrides");
                Vec::new()
            }
        }
        .into_iter()
        .filter_map(|(name, mem, cpu)| {
            if mem.is_some() || cpu.is_some() {
                Some((name, json!({ "memory_limit_mb": mem, "cpu_limit": cpu })))
            } else {
                None
            }
        })
        .collect();

    let default_mem: i64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_memory_limit_mb'")
        .fetch_one(&state.db)
        .await
        .ok()
        .and_then(|v: String| v.parse().ok())
        .unwrap_or(256);
    let default_cpu: f64 = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'default_cpu_limit'")
        .fetch_one(&state.db)
        .await
        .ok()
        .and_then(|v: String| v.parse().ok())
        .unwrap_or(0.5);
    let batch_resp = match client
        .post(&format!("{}/containers/batch-run", base_url))
        .json(&json!({
            "project_id": project_id,
            "compose_yaml": form.compose_yaml,
            "service_order": v.start_order,
            "target_services": v.target_services,
            "allow_raw_ports": p.project.allow_raw_ports,
            "docker_observe": p.docker_observe,
            "host_network": p.host_network,
            "is_background": p.project.is_background,
            "service_resources": service_resources,
            "default_memory_limit_mb": default_mem,
            "default_cpu_limit": default_cpu,
            "force_pull": true,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "remote batch-run request failed");
            let project_error = if v.target_services.is_some() {
                status::set_project_error_only(&state.db, &project_id).await
            } else {
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            };
            if let Err(e) = project_error {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("agent unreachable: {e}")})))
                .into_response();
        }
    };

    if !batch_resp.status().is_success() {
        let status_code = batch_resp.status();
        let body = match batch_resp.text().await {
            Ok(body) => body,
            Err(error) => {
                let project_error = if v.target_services.is_some() {
                    status::set_project_error_only(&state.db, &project_id).await
                } else {
                    status::transition(
                        &state.db,
                        &project_id,
                        ProjectStatus::Error,
                        &ProjectUpdateFields::default(),
                        None,
                    )
                    .await
                };
                if let Err(status_error) = project_error {
                    tracing::warn!(project_id = %project_id, error = %status_error, "compose deploy: failed to transition to Error");
                }
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error": format!("failed to read remote batch-run error response: {error}")})),
                )
                    .into_response();
            }
        };
        tracing::error!(status = %status_code, body = %body, "remote batch-run failed");
        crate::routes::manage::multi_service::apply_remote_batch_failure_metadata(&state, &project_id, &body).await;
        let project_error = if v.target_services.is_some() {
            status::set_project_error_only(&state.db, &project_id).await
        } else {
            status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                .await
        };
        if let Err(e) = project_error {
            tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
        }
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("remote batch-run failed: {body}")})))
            .into_response();
    }

    let batch_result: serde_json::Value = match batch_resp.json().await {
        Ok(v) => v,
        Err(e) => {
            let project_error = if v.target_services.is_some() {
                status::set_project_error_only(&state.db, &project_id).await
            } else {
                status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            };
            if let Err(status_error) = project_error {
                tracing::warn!(project_id = %project_id, error = %status_error, "compose deploy: failed to transition to Error");
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to parse batch-run response: {e}")})),
            )
                .into_response();
        }
    };

    let service_errors: Vec<String> = batch_result["services"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|svc| {
            svc["error"]
                .as_str()
                .map(|error| format!("{}: {}", svc["service_name"].as_str().unwrap_or("unknown"), error))
        })
        .collect();

    // Update project_services with container IDs and ports from agent response
    if let Some(services) = batch_result["services"].as_array() {
        for svc in services {
            let svc_name = svc["service_name"].as_str().unwrap_or("");
            let container_id = svc["container_id"].as_str();
            let mapped_port = svc["mapped_port"].as_u64().map(|p| p as i64);

            if let Some(cid) = container_id {
                if let Err(e) = status::set_service_running(&state.db, &project_id, svc_name, cid, mapped_port).await {
                    tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to set service running");
                }
            } else {
                if let Err(e) = status::set_service_stopped(&state.db, &project_id, svc_name).await {
                    tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to set service stopped");
                }
            }
        }

        // Set project's denormalized container_id to the public service
        let public_result = v
            .public_service
            .as_deref()
            .and_then(|name| services.iter().find(|s| s["service_name"].as_str() == Some(name)));

        if let Some(pub_svc) = public_result {
            let cid = pub_svc["container_id"].as_str().unwrap_or("").to_string();
            let port = pub_svc["mapped_port"].as_u64().map(|p| p as i64);
            if let Err(e) = status::transition(
                &state.db,
                &project_id,
                ProjectStatus::Running,
                &ProjectUpdateFields {
                    container_id: Some(Some(cid)),
                    mapped_port: Some(Some(port.unwrap_or(0))),
                    node_id: Some(target_node_id.clone()),
                    last_active_at: Some(now),
                },
                None,
            )
            .await
            {
                tracing::error!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Running");
            }
        }
    }
    if !service_errors.is_empty() {
        let _ = status::transition(&state.db, &project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
            .await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "one or more services failed to start", "service_errors": service_errors})),
        )
            .into_response();
    }
    status::derive_and_set_project_status(&state.db, &project_id).await;

    // Trigger route sync
    let _ = state.route_sync_tx.send(());

    // Clean up old per-service images by digest
    for (svc_name, digest) in &p.old_service_digests {
        let should_cleanup = v.target_services.as_ref().map_or(true, |targets| targets.contains(svc_name));
        if should_cleanup {
            crate::routes::manage::cleanup_unused_image(state, p.existing_node_id.as_deref(), digest).await;
        }
    }

    // Collect warnings from agent response
    let agent_warnings: Vec<String> = batch_result["warnings"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|val| val.as_str().map(String::from)).collect())
        .unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!({
            "status": "deployed",
            "project_id": project_id,
            "url": if v.is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", project_id, state.platform.domain())) },
            "warnings": agent_warnings,
        })),
    )
        .into_response()
}
