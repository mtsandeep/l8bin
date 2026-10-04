//! Device pairing (`/auth/device/*`) — how `l8b login` obtains a scoped
//! token without a password: the CLI prints a short-lived code, the user
//! approves it from their already-authenticated dashboard session, and the
//! CLI's next poll receives the token. `device_code` (a UUID) is the claim
//! secret; `user_code` is only the human verifier.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use litebin_common::types::TokenScope;

use crate::AppState;
use crate::auth::backend::PasswordBackend;

const CODE_TTL_SECS: i64 = 600;
const POLL_INTERVAL_SECS: u64 = 3;
/// Unambiguous alphabet (no 0/O/1/I).
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn generate_user_code() -> String {
    let mut suffix = String::with_capacity(6);
    for _ in 0..6 {
        suffix.push(CODE_ALPHABET[rand::random_range(0..CODE_ALPHABET.len())] as char);
    }
    format!("L8B-{suffix}")
}

async fn purge_expired(db: &sqlx::SqlitePool) {
    let _ = sqlx::query("DELETE FROM device_codes WHERE expires_at < ?").bind(now()).execute(db).await;
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceStartRequest {
    /// Shown on the approve page, e.g. "l8b CLI on my-laptop".
    #[serde(default)]
    pub client_name: Option<String>,
    /// Scope the client asks for; the approver makes the final call.
    #[serde(default)]
    pub scope: Option<TokenScope>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeviceStartResponse {
    /// Opaque claim secret — only the client that started the flow can poll with it.
    pub device_code: String,
    /// Human-typed verifier, e.g. `L8B-4KX2QW`.
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[utoipa::path(
    post,
    path = "/auth/device/start",
    request_body = DeviceStartRequest,
    responses((status = 200, description = "Pairing started", body = DeviceStartResponse)),
    tag = "auth",
)]
pub async fn start_device_flow(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<DeviceStartRequest>,
) -> impl IntoResponse {
    if !crate::rate_limit::allow(&headers, crate::rate_limit::Policy::DeviceStart) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error": "too many pairing requests; retry in a minute"})))
            .into_response();
    }
    purge_expired(&state.db).await;

    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM device_codes WHERE status = 'pending' AND expires_at > ?")
            .bind(now())
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
    if pending > 100 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "too many pending pairing requests; retry shortly"})),
        )
            .into_response();
    }

    let user_code = generate_user_code();
    let id = uuid::Uuid::new_v4().to_string();
    let suggested_scope = payload.scope.unwrap_or_default();

    if let Err(e) = sqlx::query(
        "INSERT INTO device_codes (id, user_code, client_name, suggested_scope, status, expires_at, created_at) VALUES (?, ?, ?, ?, 'pending', ?, ?)",
    )
    .bind(&id)
    .bind(&user_code)
    .bind(&payload.client_name)
    .bind(suggested_scope)
    .bind(now() + CODE_TTL_SECS)
    .bind(now())
    .execute(&state.db)
    .await
    {
        tracing::error!(error = %e, "device pairing: failed to insert code");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "failed to start pairing"})),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(DeviceStartResponse {
            device_code: id,
            user_code,
            verification_url: "/connect".to_string(),
            expires_in: CODE_TTL_SECS,
            interval: POLL_INTERVAL_SECS,
        }),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct DeviceLookupQuery {
    pub user_code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeviceRequestInfo {
    pub user_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    pub suggested_scope: TokenScope,
    pub expires_at: i64,
}

#[utoipa::path(
    get,
    path = "/auth/device",
    params(("user_code" = String, Query, description = "Code being approved")),
    responses(
        (status = 200, description = "Pending pairing request", body = DeviceRequestInfo),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "No pending request for that code"),
    ),
    tag = "auth",
    security(("session_auth" = [])),
)]
pub async fn lookup_device_request(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Query(q): Query<DeviceLookupQuery>,
) -> impl IntoResponse {
    if auth_session.user.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }

    let row: Option<(Option<String>, String, i64)> = sqlx::query_as(
        "SELECT client_name, suggested_scope, expires_at FROM device_codes WHERE user_code = ? AND status = 'pending'",
    )
    .bind(q.user_code.trim().to_uppercase())
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    match row {
        Some((client_name, suggested_scope, expires_at)) if expires_at > now() => {
            let scope = TokenScope::parse_or_default(&suggested_scope);
            (
                StatusCode::OK,
                Json(DeviceRequestInfo { user_code: q.user_code, client_name, suggested_scope: scope, expires_at }),
            )
                .into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"error": "no pending request for that code"}))).into_response(),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceApproveRequest {
    pub user_code: String,
    pub approve: bool,
    pub scope: TokenScope,
    /// Optional: bind the token to a single project.
    #[serde(default)]
    pub project_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/auth/device/approve",
    request_body = DeviceApproveRequest,
    responses(
        (status = 200, description = "Approval recorded"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "No pending request for that code"),
    ),
    tag = "auth",
    security(("session_auth" = [])),
)]
pub async fn approve_device_request(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<DeviceApproveRequest>,
) -> impl IntoResponse {
    let Some(user) = auth_session.user else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    };

    // A project binding must reference a real project.
    if let Some(ref pid) = payload.project_id {
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM projects WHERE id = ?")
            .bind(pid)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
        if exists.is_none() {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("project '{pid}' not found")})))
                .into_response();
        }
    }

    let status = if payload.approve { "approved" } else { "denied" };
    let result = sqlx::query(
        "UPDATE device_codes SET status = ?, user_id = ?, scope = ?, project_id = ? WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(status)
    .bind(&user.id)
    .bind(payload.scope)
    .bind(&payload.project_id)
    .bind(payload.user_code.trim().to_uppercase())
    .bind(now())
    .execute(&state.db)
    .await;

    match result {
        Ok(r) if r.rows_affected() == 1 => {
            tracing::info!(
                user = %user.username,
                code = %payload.user_code,
                approved = payload.approve,
                scope = %payload.scope,
                project_id = ?payload.project_id,
                "device pairing decision"
            );
            (StatusCode::OK, Json(json!({"status": status}))).into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "no pending request for that code"}))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "device pairing: approve failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "database error"}))).into_response()
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceTokenRequest {
    pub device_code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeviceTokenResponse {
    /// pending | ok | denied | expired
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TokenScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/auth/device/token",
    request_body = DeviceTokenRequest,
    responses((status = 200, description = "Poll result; token present once when approved", body = DeviceTokenResponse)),
    tag = "auth",
)]
pub async fn poll_device_token(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<DeviceTokenRequest>,
) -> impl IntoResponse {
    if !crate::rate_limit::allow(&headers, crate::rate_limit::Policy::DeviceToken) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error": "polling too fast; retry in a minute"})))
            .into_response();
    }
    purge_expired(&state.db).await;

    let row: Option<(String, String, Option<String>, String, i64)> =
        sqlx::query_as("SELECT status, user_id, project_id, scope, expires_at FROM device_codes WHERE id = ?")
            .bind(&payload.device_code)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some((status, user_id, project_id, scope, expires_at)) = row else {
        return (
            StatusCode::OK,
            Json(DeviceTokenResponse { status: "expired".into(), token: None, scope: None, project_id: None }),
        )
            .into_response();
    };

    let response = match status.as_str() {
        "pending" if expires_at > now() => {
            DeviceTokenResponse { status: "pending".into(), token: None, scope: None, project_id: None }
        }
        "denied" => DeviceTokenResponse { status: "denied".into(), token: None, scope: None, project_id: None },
        "approved" if expires_at > now() => {
            let scope = TokenScope::parse_or_default(&scope);
            match mint_pairing_token(&state.db, &user_id, project_id.as_deref(), scope).await {
                Ok(token) => {
                    let claimed =
                        sqlx::query("UPDATE device_codes SET status = 'claimed' WHERE id = ? AND status = 'approved'")
                            .bind(&payload.device_code)
                            .execute(&state.db)
                            .await
                            .map(|r| r.rows_affected() == 1)
                            .unwrap_or(false);
                    if !claimed {
                        // Concurrent poll won the claim; this token is dead but unused — revoke it.
                        let hash = hex::encode(Sha256::digest(token.as_bytes()));
                        let _ = sqlx::query("DELETE FROM deploy_tokens WHERE token_hash = ?")
                            .bind(hash)
                            .execute(&state.db)
                            .await;
                        return (
                            StatusCode::OK,
                            Json(DeviceTokenResponse {
                                status: "expired".into(),
                                token: None,
                                scope: None,
                                project_id: None,
                            }),
                        )
                            .into_response();
                    }
                    tracing::info!(user_id = %user_id, scope = %scope, project_id = ?project_id, "device pairing token issued");
                    DeviceTokenResponse { status: "ok".into(), token: Some(token), scope: Some(scope), project_id }
                }
                Err(e) => {
                    tracing::error!(error = %e, "device pairing: token mint failed");
                    DeviceTokenResponse { status: "expired".into(), token: None, scope: None, project_id: None }
                }
            }
        }
        _ => DeviceTokenResponse { status: "expired".into(), token: None, scope: None, project_id: None },
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// Mint a deploy token bound to the approval. Plaintext is returned once.
async fn mint_pairing_token(
    db: &sqlx::SqlitePool,
    user_id: &str,
    project_id: Option<&str>,
    scope: TokenScope,
) -> Result<String, sqlx::Error> {
    let token_bytes: [u8; 32] = rand::random();
    let token = hex::encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let name = match project_id {
        Some(pid) => format!("paired ({pid})"),
        None => "paired".to_string(),
    };

    sqlx::query(
        "INSERT INTO deploy_tokens (id, user_id, project_id, token_hash, name, created_at, scope) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(project_id)
    .bind(&token_hash)
    .bind(&name)
    .bind(now())
    .bind(scope)
    .execute(db)
    .await?;

    Ok(token)
}
