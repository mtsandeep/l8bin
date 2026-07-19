use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::db::models::{DeployToken, DeployTokenResponse};

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateTokenRequest {
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CreateTokenResponse {
    pub token: String,
    pub token_info: DeployTokenResponse,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ListTokensQuery {
    pub project_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/deploy-tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Token created", body = CreateTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy-tokens",
    security(("session_auth" = [])),
)]
pub async fn create_token(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"})))
                .into_response();
        }
    };

    // If project-scoped, verify project belongs to user
    if let Some(ref pid) = payload.project_id {
        let project: Option<(String,)> = sqlx::query_as("SELECT id FROM projects WHERE id = ? AND user_id = ?")
            .bind(pid)
            .bind(&user_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

        if project.is_none() {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Project not found"}))).into_response();
        }
    }

    // Generate 32-byte random token (64 hex chars, 256-bit entropy)
    let token_bytes: [u8; 32] = rand::random();
    let token = hex::encode(token_bytes);

    // Hash with SHA-256 for storage
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let now = chrono::Utc::now().timestamp();
    let token_id = uuid::Uuid::new_v4().to_string();

    let _ = sqlx::query(
        "INSERT INTO deploy_tokens (id, user_id, project_id, token_hash, name, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&token_id)
    .bind(&user_id)
    .bind(&payload.project_id)
    .bind(&token_hash)
    .bind(&payload.name)
    .bind(now)
    .bind(payload.expires_at)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "failed to create deploy token");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to create token: {e}")})),
        )
            .into_response();
    });

    let token_info = DeployTokenResponse {
        id: token_id,
        name: payload.name,
        project_id: payload.project_id,
        last_used_at: None,
        expires_at: payload.expires_at,
        created_at: now,
    };

    (StatusCode::CREATED, Json(CreateTokenResponse { token, token_info })).into_response()
}

#[utoipa::path(
    get,
    path = "/deploy-tokens",
    params(
        ("project_id" = Option<String>, Query, description = "Filter by project ID"),
    ),
    responses(
        (status = 200, description = "List of deploy tokens", body = Vec<crate::db::models::DeployTokenResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "deploy-tokens",
    security(("session_auth" = [])),
)]
pub async fn list_tokens(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Query(params): Query<ListTokensQuery>,
) -> impl IntoResponse {
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"})))
                .into_response();
        }
    };

    let tokens: Vec<DeployToken> = if let Some(ref pid) = params.project_id {
        // Show global tokens + tokens scoped to this project
        sqlx::query_as(
            "SELECT * FROM deploy_tokens WHERE user_id = ? AND (project_id IS NULL OR project_id = ?) ORDER BY created_at DESC",
        )
        .bind(&user_id)
        .bind(pid)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    } else {
        // No filter — show all tokens for this user
        sqlx::query_as("SELECT * FROM deploy_tokens WHERE user_id = ? ORDER BY created_at DESC")
            .bind(&user_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
    };

    let response: Vec<DeployTokenResponse> = tokens.into_iter().map(Into::into).collect();
    Json(response).into_response()
}

#[utoipa::path(
    delete,
    path = "/deploy-tokens/{id}",
    params(
        ("id" = String, Path, description = "Token ID"),
    ),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Token not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy-tokens",
    security(("session_auth" = [])),
)]
pub async fn revoke_token(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    axum::extract::Path(token_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let user_id = match auth_session.user {
        Some(u) => u.id,
        None => {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"})))
                .into_response();
        }
    };

    let result = match sqlx::query("DELETE FROM deploy_tokens WHERE id = ? AND user_id = ?")
        .bind(&token_id)
        .bind(&user_id)
        .execute(&state.db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(token_id = %token_id, error = %e, "failed to revoke deploy token");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "database error"})))
                .into_response();
        }
    };

    if result.rows_affected() == 0 {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Token not found"}))).into_response();
    }

    (StatusCode::NO_CONTENT, "").into_response()
}
