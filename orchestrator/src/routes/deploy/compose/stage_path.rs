use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::ProjectStatus;

use super::form::ComposeForm;
use super::persist::PersistedCompose;
use super::validate::ValidatedCompose;

/// First-deploy staging: prepare compose + runtime .env (via remote agent
/// batch-run with `stage_only` when not local), do not start containers.
pub(super) async fn stage_only_path(
    state: &AppState,
    form: &ComposeForm,
    v: &ValidatedCompose,
    p: &PersistedCompose,
) -> axum::response::Response {
    let project_id = &form.project_id;
    let target_node_id = &p.target_node_id;

    if target_node_id != "local" {
        let agent = match nodes::client::AgentClient::resolve(state, target_node_id).await {
            Ok(a) => a,
            Err(e) => {
                if let Err(e) = status::transition(
                    &state.db,
                    project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await
                {
                    tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
                }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client unavailable: {e}")})),
                )
                    .into_response();
            }
        };

        let payload = crate::routes::manage::multi_service::build_batch_run_payload(
            &state.db,
            crate::routes::manage::multi_service::BatchRunInputs {
                project_id: project_id.clone(),
                compose_yaml: form.compose_yaml.clone(),
                service_order: v.start_order.clone(),
                target_services: None,
                allow_raw_ports: None,
                docker_observe: Some(p.docker_observe),
                host_network: Some(p.host_network),
                is_background: v.is_background,
                force_pull: false,
                stage_only: true,
            },
        )
        .await;
        if let Err(e) = agent.batch_run(&payload).await {
            if let nodes::client::AgentClientError::Status { body, .. } = &e {
                tracing::error!(body = %body, "remote compose stage failed");
                if let Err(transition_error) = status::transition(
                    &state.db,
                    project_id,
                    ProjectStatus::Error,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await
                {
                    tracing::warn!(project_id = %project_id, error = %transition_error, "compose stage: failed to transition to Error");
                }
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("remote stage failed: {body}")})),
                )
                    .into_response();
            }
            tracing::error!(error = %e, "remote compose stage request failed");
            if let Err(transition_error) =
                status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            {
                tracing::warn!(project_id = %project_id, error = %transition_error, "compose stage: failed to transition to Error");
            }
            let (status_code, message) = e.into_response_parts();
            return (status_code, Json(json!({"error": message}))).into_response();
        }
    }

    if let Err(e) =
        status::transition(&state.db, project_id, ProjectStatus::Unconfigured, &ProjectUpdateFields::default(), None)
            .await
    {
        tracing::error!(project_id = %project_id, error = %e, "compose stage: failed to mark project unconfigured");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "failed to persist staged deployment status"})),
        )
            .into_response();
    }

    tracing::info!(
        project_id = %project_id,
        node_id = %target_node_id,
        "compose deployment staged; awaiting runtime configuration"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "unconfigured",
            "project_id": project_id,
            "node_id": target_node_id,
            "url": if v.is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", project_id, state.platform.domain())) },
            "message": "Deployment staged. Configure runtime secrets, then start the project.",
        })),
    )
        .into_response()
}
