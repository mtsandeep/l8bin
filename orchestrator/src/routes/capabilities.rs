//! Compose validation and project capability HTTP handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use compose_bollard::{analyze_compose_yaml, CompatibilityReport};
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
        match analyze_compose_yaml(
            &payload.compose,
            if payload.is_background {
                None
            } else {
                payload.public_service.as_deref()
            },
            payload.project_id.as_deref(),
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
    let granted_by = auth_session.user.map(|u| u.id);
    capabilities::grant_many(&state.db, &id, &caps, granted_by.as_deref())
        .await
        .map_err(capabilities::db_err)?;
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
    capabilities::revoke(&state.db, &id, cap)
        .await
        .map_err(capabilities::db_err)?;
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
