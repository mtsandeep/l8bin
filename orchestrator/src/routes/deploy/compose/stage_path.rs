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
        let node = match crate::routes::manage::get_node_from_db(&state.db, target_node_id).await {
            Ok(n) => n,
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
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("{:?}", e)}))).into_response();
            }
        };

        let client = match nodes::client::get_node_client(&state.node_clients, target_node_id) {
            Ok(c) => c,
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
                    Json(json!({"error": format!("node client unavailable: {:?}", e)})),
                )
                    .into_response();
            }
        };

        let base_url = agent_base_url(&state.config, &node);
        let stage_resp = match client
            .post(format!("{}/containers/batch-run", base_url))
            .json(&json!({
                "project_id": project_id,
                "compose_yaml": form.compose_yaml,
                "service_order": v.start_order,
                "is_background": v.is_background,
                "docker_observe": p.docker_observe,
                "host_network": p.host_network,
                "stage_only": true,
            }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "remote compose stage request failed");
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
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("agent unreachable: {e}")})))
                    .into_response();
            }
        };

        if !stage_resp.status().is_success() {
            let body = stage_resp.text().await.unwrap_or_default();
            tracing::error!(body = %body, "remote compose stage failed");
            if let Err(e) =
                status::transition(&state.db, project_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await
            {
                tracing::warn!(project_id = %project_id, error = %e, "compose stage: failed to transition to Error");
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("remote stage failed: {body}")})))
                .into_response();
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
