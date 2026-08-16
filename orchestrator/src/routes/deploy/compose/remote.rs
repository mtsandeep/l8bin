use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::AppState;
use crate::nodes;
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

    let agent = match nodes::client::AgentClient::resolve(state, target_node_id).await {
        Ok(a) => a,
        Err(e) => {
            if let Err(e) =
                status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("node client unavailable: {e}")})))
                .into_response();
        }
    };

    let payload = crate::routes::manage::multi_service::build_batch_run_payload(
        &state.db,
        crate::routes::manage::multi_service::BatchRunInputs {
            project_id: project_id.clone(),
            compose_yaml: form.compose_yaml.clone(),
            service_order: v.start_order.clone(),
            target_services: v.target_services.clone(),
            allow_raw_ports: Some(p.project.allow_raw_ports),
            docker_observe: Some(p.docker_observe),
            host_network: Some(p.host_network),
            is_background: p.project.is_background,
            force_pull: true,
            stage_only: false,
        },
    )
    .await;

    let batch_result = match agent.batch_run(&payload).await {
        Ok(resp) => resp,
        Err(nodes::client::AgentClientError::Status { body, .. }) => {
            tracing::error!(body = %body, "remote batch-run failed");
            crate::routes::manage::multi_service::apply_remote_batch_failure_metadata(state, project_id, &body).await;
            let project_error = if v.target_services.is_some() {
                status::set_project_error_only(&state.db, project_id).await
            } else {
                status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            };
            if let Err(e) = project_error {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": format!("remote batch-run failed: {body}")})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "remote batch-run request failed");
            let project_error = if v.target_services.is_some() {
                status::set_project_error_only(&state.db, project_id).await
            } else {
                status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            };
            if let Err(e) = project_error {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to transition to Error");
            }
            let (status_code, message) = e.into_response_parts();
            return (status_code, Json(json!({"error": message}))).into_response();
        }
    };

    let service_errors: Vec<String> = batch_result
        .services
        .iter()
        .filter_map(|svc| svc.error.as_deref().map(|error| format!("{}: {error}", svc.service_name)))
        .collect();

    // Update project_services with container IDs and ports from agent response
    for svc in &batch_result.services {
        let mapped_port = svc.mapped_port.map(i64::from);
        if let Some(cid) = svc.container_id.as_deref() {
            if let Err(e) =
                status::set_service_running(&state.db, project_id, &svc.service_name, cid, mapped_port).await
            {
                tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "compose deploy: failed to set service running");
            }
        } else {
            if let Err(e) = status::set_service_stopped(&state.db, project_id, &svc.service_name).await {
                tracing::warn!(project_id = %project_id, service = %svc.service_name, error = %e, "compose deploy: failed to set service stopped");
            }
        }
    }

    // Set project's denormalized container_id to the public service
    let public_result =
        v.public_service.as_deref().and_then(|name| batch_result.services.iter().find(|s| s.service_name == name));

    if let Some(pub_svc) = public_result {
        let cid = pub_svc.container_id.clone().unwrap_or_default();
        let port = pub_svc.mapped_port.map(i64::from);
        if let Err(e) = status::transition(
            &state.db,
            project_id,
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
    if !service_errors.is_empty() {
        let _ = status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
            .await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "one or more services failed to start", "service_errors": service_errors})),
        )
            .into_response();
    }
    status::derive_and_set_project_status(&state.db, project_id).await;

    // Trigger route sync
    let _ = state.route_sync_tx.send(());

    // Clean up old per-service images by digest
    for (svc_name, digest) in &p.old_service_digests {
        let should_cleanup = v.target_services.as_ref().is_none_or(|targets| targets.contains(svc_name));
        if should_cleanup {
            crate::routes::manage::cleanup_unused_image(state, p.existing_node_id.as_deref(), digest).await;
        }
    }

    // Collect warnings from agent response
    let agent_warnings: Vec<String> = batch_result.warnings;

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
