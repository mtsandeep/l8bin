mod form;
mod local;
mod persist;
mod preflight;
mod remote;
mod stage_path;
mod validate;

use axum::extract::Multipart;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde_json::json;

use crate::AppState;
use crate::auth::backend::PasswordBackend;

use form::ComposeForm;
use persist::persist_compose_deploy;
use stage_path::stage_only_path;
use validate::validate_compose_request;

#[utoipa::path(
    post,
    path = "/deploy/compose",
    request_body(content = String, description = "Multipart form with project_id and compose file"),
    responses(
        (status = 200, description = "Compose deployment started"),
        (status = 400, description = "Missing project_id"),
        (status = 401, description = "Authentication required"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy",
    security(("session_auth" = []), ("bearer_token" = [])),
)]
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
/// - `is_background` (optional text field, "true"/"false")
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
    let mut is_background = None;
    let mut custom_domain = None;
    let mut allow_raw_ports = None;
    let mut grant_capabilities_raw = None;
    let mut compose_content = None;
    let mut target_services_raw = None;
    let mut stage_only_requested = false;

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
            "is_background" => {
                is_background = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "custom_domain" => {
                custom_domain = field.text().await.ok();
            }
            "allow_raw_ports" => {
                allow_raw_ports = field.text().await.ok().and_then(|v| v.parse::<bool>().ok());
            }
            "grant_capabilities" => {
                grant_capabilities_raw = field.text().await.ok();
            }
            "compose" => {
                compose_content = field.bytes().await.ok();
            }
            "target_services" => {
                target_services_raw = field.text().await.ok();
            }
            "stage_only" => {
                stage_only_requested = field.text().await.ok().and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
            }
            _ => {
                tracing::debug!(field = %field_name, "ignoring unknown multipart field");
            }
        }
    }

    let project_id = match project_id {
        Some(id) if !id.is_empty() => id,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "project_id is required"}))).into_response(),
    };

    let compose_bytes = match compose_content {
        Some(b) if !b.is_empty() => b,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "compose file is required"}))).into_response(),
    };

    let compose_yaml = match String::from_utf8(compose_bytes.to_vec()) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("compose file is not valid UTF-8: {e}")})))
                .into_response();
        }
    };

    // Authenticate
    let user_id = match auth_session.user {
        Some(u) => u.id.clone(),
        None => match crate::auth::extract_deploy_token(&state, &headers, &project_id).await {
            Some(uid) => uid,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Authentication required. Use session login or provide a deploy token."})),
                )
                    .into_response();
            }
        },
    };

    let form = ComposeForm {
        project_id,
        name,
        description,
        node_id,
        auto_stop_enabled,
        auto_stop_timeout_mins,
        auto_start_enabled,
        is_background,
        custom_domain,
        allow_raw_ports,
        grant_capabilities_raw,
        compose_yaml,
        target_services_raw,
        stage_only_requested,
    };

    let v = match validate_compose_request(&state, &form).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let now = chrono::Utc::now().timestamp();

    let p = match persist_compose_deploy(&state, &user_id, &form, &v, now).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // First-deploy staging: prepare compose + runtime .env, do not start containers.
    if v.stage_only {
        return stage_only_path(&state, &form, &v, &p).await;
    }

    // Local vs remote deploy path
    if p.target_node_id != "local" {
        return remote::remote_deploy_path(&state, &form, &v, &p, now).await;
    }

    // --- Local path: spawn background task for heavy lifting ---
    let state_clone = state.clone();
    let project_id_clone = form.project_id.clone();
    let project_clone = p.project.clone();
    let compose_clone = v.compose.clone();
    let start_order_clone = v.start_order.clone();
    let target_node_id_clone = p.target_node_id.clone();
    let target_services_clone = v.target_services.clone();
    let old_service_digests_clone = p.old_service_digests.clone();
    let existing_node_id_clone = p.existing_node_id.clone();

    tokio::spawn(async move {
        crate::routes::deploy::logs::push_deploy_log(&state_clone, &project_id_clone, "Compose deployment started");

        let result: Result<(), anyhow::Error> = local::run_local_compose_deploy(
            state_clone.clone(),
            project_id_clone.clone(),
            project_clone,
            compose_clone,
            start_order_clone,
            target_node_id_clone,
            target_services_clone.clone(),
            old_service_digests_clone,
            existing_node_id_clone,
        )
        .await;

        if let Err(e) = result {
            tracing::error!(project_id = %project_id_clone, error = %e, "background compose deploy failed");
            crate::routes::deploy::logs::push_deploy_log(
                &state_clone,
                &project_id_clone,
                &format!("Deploy failed: {}", e),
            );
            local::persist_background_failure(&state_clone, &project_id_clone, target_services_clone.as_ref()).await;
        }
    });

    // Explain the fail-closed translation when observation was not granted.
    let sock_warnings: Vec<String> = if !p.docker_observe && form.compose_yaml.contains("/docker.sock") {
        vec!["Docker socket declaration found without docker-observe — the raw socket was removed".into()]
    } else {
        vec![]
    };

    (
        StatusCode::OK,
        Json(json!({
            "status": "deploying",
            "project_id": form.project_id,
            "url": if v.is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", form.project_id, state.platform.domain())) },
            "message": "Compose deployment started in background",
            "warnings": sock_warnings,
        })),
    )
        .into_response()
}
