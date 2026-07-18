//! Compose validation and project capability HTTP handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use compose_bollard::{analyze_compose_yaml_for_workload, CompatibilityReport};
use litebin_common::capabilities::{
    capability_catalog, parse_capability_ids, CapabilityInfo, ProjectCapability,
    ProjectCapabilityStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::backend::PasswordBackend;
use crate::{capabilities, AppState};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ValidateComposeRequest {
    pub compose: String,
    /// Optional explicit public service name.
    pub public_service: Option<String>,
    /// Background projects have no public Compose service.
    #[serde(default)]
    pub is_background: bool,
    /// Existing project id — used to compute missing grants.
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ValidateComposeResponse {
    pub report: CompatibilityReport,
    /// Capability ids required by this file that are not yet granted
    /// (empty when no project_id, or when all are granted).
    pub missing_capabilities: Vec<String>,
    pub catalog: Vec<CapabilityInfo>,
}

#[utoipa::path(
    post,
    path = "/compose/validate",
    request_body = ValidateComposeRequest,
    responses(
        (status = 200, description = "Compatibility report"),
        (status = 400),
        (status = 401),
    ),
    tag = "compose",
    security(("session_auth" = []), ("bearer_token" = []))
)]
pub async fn validate_compose(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<ValidateComposeRequest>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        let pid = payload.project_id.as_deref().unwrap_or("");
        if crate::auth::extract_deploy_token(&state, &headers, pid)
            .await
            .is_none()
        {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Authentication required"})),
            )
                .into_response();
        }
    }

    let (_compose, report) =
        match analyze_compose_yaml_for_workload(
            &payload.compose,
            if payload.is_background {
                None
            } else {
                payload.public_service.as_deref()
            },
            payload.project_id.as_deref(),
            payload.is_background,
        ) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        };

    let missing = if let Some(ref project_id) = payload.project_id {
        match capabilities::granted_ids(&state.db, project_id).await {
            Ok(granted) => capabilities::missing_capabilities(&report.required_capabilities, &granted),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("failed to read grants: {e}")})),
                )
                    .into_response();
            }
        }
    } else {
        report.required_capabilities.clone()
    };

    (
        StatusCode::OK,
        Json(ValidateComposeResponse {
            report,
            missing_capabilities: missing,
            catalog: capability_catalog(),
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantCapabilitiesRequest {
    pub capabilities: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/projects/{id}/capabilities",
    params(("id" = String, Path)),
    responses((status = 200, body = Vec<ProjectCapabilityStatus>), (status = 404)),
    tag = "capabilities",
    security(("session_auth" = []))
)]
pub async fn list_project_capabilities(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ProjectCapabilityStatus>>, (StatusCode, String)> {
    ensure_project(&state, &id).await?;
    let list = capabilities::status_list_for_project(&state.db, &id)
        .await
        .map_err(capabilities::db_err)?;
    Ok(Json(list))
}

#[utoipa::path(
    post,
    path = "/projects/{id}/capabilities",
    params(("id" = String, Path)),
    request_body = GrantCapabilitiesRequest,
    responses((status = 200, body = Vec<ProjectCapabilityStatus>), (status = 400), (status = 404)),
    tag = "capabilities",
    security(("session_auth" = []))
)]
pub async fn grant_project_capabilities(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<GrantCapabilitiesRequest>,
) -> Result<Json<Vec<ProjectCapabilityStatus>>, (StatusCode, String)> {
    ensure_project(&state, &id).await?;
    let caps = parse_capability_ids(&payload.capabilities).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if caps.contains(&ProjectCapability::HostNetwork) {
        let is_background: bool = sqlx::query_scalar("SELECT is_background FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .map_err(capabilities::db_err)?;
        if !is_background {
            return Err((StatusCode::BAD_REQUEST, "host networking is restricted to background projects".into()));
        }
    }
    let granted_by = auth_session.user.map(|u| u.id);
    capabilities::grant_many(&state.db, &id, &caps, granted_by.as_deref())
        .await
        .map_err(capabilities::db_err)?;
    if caps.iter().any(|cap| matches!(cap, ProjectCapability::DockerObserve | ProjectCapability::HostNetwork)) {
        let node_id: Option<String> = sqlx::query_scalar("SELECT node_id FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .map_err(capabilities::db_err)?;
        if let Some(node_id) = node_id {
            if node_id != "local" {
                crate::cloudflare_router::push_project_meta_to_agent(
                    &node_id,
                    &state.db,
                    &state.node_clients,
                    &state.config,
                )
                .await;
            }
        }
    }
    let list = capabilities::status_list_for_project(&state.db, &id)
        .await
        .map_err(capabilities::db_err)?;
    Ok(Json(list))
}

#[utoipa::path(
    delete,
    path = "/projects/{id}/capabilities/{capability}",
    params(
        ("id" = String, Path),
        ("capability" = String, Path),
    ),
    responses((status = 200, body = Vec<ProjectCapabilityStatus>), (status = 400), (status = 404)),
    tag = "capabilities",
    security(("session_auth" = []))
)]
pub async fn revoke_project_capability(
    State(state): State<AppState>,
    Path((id, capability)): Path<(String, String)>,
) -> Result<Json<Vec<ProjectCapabilityStatus>>, (StatusCode, String)> {
    ensure_project(&state, &id).await?;
    let cap = ProjectCapability::parse(&capability).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("unknown capability '{capability}'"),
        )
    })?;
    let mut docker_observe_node_id: Option<String> = None;
    let mut host_network_node_id: Option<String> = None;
    if cap == ProjectCapability::HostNetwork {
        let node_id: Option<String> = sqlx::query_scalar("SELECT node_id FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .map_err(capabilities::db_err)?;
        host_network_node_id = node_id.clone();
        let container_ids: Vec<String> = sqlx::query_scalar(
            "SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL",
        )
        .bind(&id)
        .fetch_all(&state.db)
        .await
        .map_err(capabilities::db_err)?;
        if node_id.as_deref().unwrap_or("local") == "local" {
            for container_id in &container_ids {
                if let Err(error) = state.docker.stop_container(container_id).await {
                    if litebin_common::docker::DockerErrorKind::from_anyhow(&error)
                        != litebin_common::docker::DockerErrorKind::NotFound
                    {
                        return Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to stop host-network workload before revocation: {error}"),
                        ));
                    }
                }
            }
        } else if let Some(node_id) = node_id.as_deref() {
            let node = crate::routes::manage::get_node_from_db(&state.db, node_id)
                .await
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("{e:?}")))?;
            let client = crate::nodes::client::get_node_client(&state.node_clients, node_id)
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("{e:?}")))?;
            let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
            for container_id in &container_ids {
                let response = client
                    .post(format!("{base_url}/containers/stop"))
                    .json(&json!({"container_id": container_id}))
                    .send()
                    .await
                    .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("failed to stop host-network workload: {e}")))?;
                if !response.status().is_success() {
                    return Err((StatusCode::BAD_GATEWAY, "failed to stop host-network workload before revocation".into()));
                }
            }
        }
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE project_services SET status = 'stopped' WHERE project_id = ? AND is_oneshot = 0")
            .bind(&id)
            .execute(&state.db)
            .await
            .map_err(capabilities::db_err)?;
        sqlx::query("UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&id)
            .execute(&state.db)
            .await
            .map_err(capabilities::db_err)?;
    }
    if cap == ProjectCapability::DockerObserve {
        let node_id: Option<String> = sqlx::query_scalar("SELECT node_id FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .map_err(capabilities::db_err)?;
        docker_observe_node_id = node_id.clone();
        let proxy_name = litebin_common::types::container_name(
            &id,
            litebin_common::types::DOCKER_PROXY_SERVICE,
            None,
        );
        if node_id.as_deref().unwrap_or("local") == "local" {
            state
                .docker
                .remove_by_service_name(&id, litebin_common::types::DOCKER_PROXY_SERVICE, None)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to remove Docker observation proxy: {e}")))?;
        } else {
            let node_id = node_id.as_deref().unwrap();
            let node = crate::routes::manage::get_node_from_db(&state.db, node_id)
                .await
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("{e:?}")))?;
            let client = crate::nodes::client::get_node_client(&state.node_clients, node_id)
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("{e:?}")))?;
            let base_url = crate::routes::manage::agent_base_url(&state.config, &node);
            let response = client
                .post(format!("{base_url}/containers/remove"))
                .json(&json!({"container_id": proxy_name}))
                .send()
                .await
                .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("failed to contact agent: {e}")))?;
            if !response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("failed to remove Docker observation proxy: {body}"),
                ));
            }
        }
    }
    capabilities::revoke(&state.db, &id, cap)
        .await
        .map_err(capabilities::db_err)?;
    if let Some(node_id) = docker_observe_node_id {
        if node_id != "local" {
            crate::cloudflare_router::push_project_meta_to_agent(
                &node_id,
                &state.db,
                &state.node_clients,
                &state.config,
            )
            .await;
        }
    }
    if let Some(node_id) = host_network_node_id {
        if node_id != "local" {
            crate::cloudflare_router::push_project_meta_to_agent(
                &node_id,
                &state.db,
                &state.node_clients,
                &state.config,
            )
            .await;
        }
    }
    let list = capabilities::status_list_for_project(&state.db, &id)
        .await
        .map_err(capabilities::db_err)?;
    Ok(Json(list))
}

async fn ensure_project(state: &AppState, id: &str) -> Result<(), (StatusCode, String)> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if exists.is_none() {
        return Err((StatusCode::NOT_FOUND, "project not found".into()));
    }
    Ok(())
}
