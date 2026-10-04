//! Scope-ladder guards: a session, or a Bearer token with sufficient scope.
//! Project-scoped tokens are confined to their own project's paths.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_login::AuthSession;
use serde_json::json;
use sha2::{Digest, Sha256};

use litebin_common::types::TokenScope;

use crate::AppState;
use crate::auth::backend::PasswordBackend;

/// Set on Bearer-auth requests; absent for sessions.
#[derive(Debug, Clone)]
pub struct TokenContext {
    pub user_id: String,
    pub scope: TokenScope,
    pub project_id: Option<String>,
    pub token_id: String,
    pub token_name: Option<String>,
}

fn unauthorized(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error": message}))).into_response()
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": message}))).into_response()
}

/// Look up a Bearer token by hash. Validates expiry only; scope and project
/// binding are the caller's decision.
async fn load_bearer_token(state: &AppState, headers: &HeaderMap) -> Option<crate::db::models::DeployToken> {
    let auth_header = headers.get("authorization")?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let now = chrono::Utc::now().timestamp();

    let row: Option<crate::db::models::DeployToken> =
        sqlx::query_as("SELECT * FROM deploy_tokens WHERE token_hash = ? AND (expires_at IS NULL OR expires_at > ?)")
            .bind(&token_hash)
            .bind(now)
            .fetch_optional(&state.db)
            .await
            .ok()?;

    if let Some(t) = &row {
        let now = chrono::Utc::now().timestamp();
        if let Err(e) = sqlx::query("UPDATE deploy_tokens SET last_used_at = ? WHERE id = ?")
            .bind(now)
            .bind(&t.id)
            .execute(&state.db)
            .await
        {
            tracing::warn!(token_id = %t.id, error = %e, "auth: failed to update deploy token last_used_at");
        }
    }

    row
}

/// Project tokens: their own `/projects/{id}/…` subtree plus `/meta`;
/// cross-project views need a global token.
fn path_allows_project_token(path: &str, bound_project: &str) -> bool {
    if path == "/meta" || path == "/whoami" {
        return true;
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.first() == Some(&"projects") && segments.len() >= 2 {
        return segments[1] == bound_project;
    }
    false
}

async fn authorize(
    required: TokenScope,
    auth_session: &AuthSession<PasswordBackend>,
    state: &AppState,
    request: &mut axum::extract::Request,
) -> Result<(), Response> {
    // Session always passes.
    if auth_session.user.is_some() {
        return Ok(());
    }

    let token = match load_bearer_token(state, request.headers()).await {
        Some(t) => t,
        None => {
            let has_bearer = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("Bearer "));
            return Err(if has_bearer {
                unauthorized("Invalid or expired deploy token")
            } else {
                unauthorized("Authentication required. Use session login or provide a deploy token.")
            });
        }
    };

    if token.scope < required {
        return Err(forbidden(&format!(
            "deploy token scope '{}' is insufficient; this endpoint requires '{}'",
            token.scope, required
        )));
    }

    if let Some(bound) = &token.project_id {
        let path = request.uri().path();
        if !path_allows_project_token(path, bound) {
            return Err(forbidden(&format!(
                "deploy token is scoped to project '{bound}' and cannot access this endpoint"
            )));
        }
    }

    request.extensions_mut().insert(TokenContext {
        user_id: token.user_id,
        scope: token.scope,
        project_id: token.project_id,
        token_id: token.id,
        token_name: token.name,
    });
    Ok(())
}

macro_rules! scope_guard {
    ($name:ident, $required:expr, $doc:literal) => {
        #[doc = $doc]
        pub async fn $name(
            auth_session: AuthSession<PasswordBackend>,
            State(state): State<AppState>,
            mut request: axum::extract::Request,
            next: Next,
        ) -> Response {
            match authorize($required, &auth_session, &state, &mut request).await {
                Ok(()) => next.run(request).await,
                Err(response) => response,
            }
        }
    };
}

scope_guard!(require_read, TokenScope::Read, "Guard: session or token with scope ≥ read.");
scope_guard!(require_manage, TokenScope::Manage, "Guard: session or token with scope ≥ manage.");
scope_guard!(require_admin, TokenScope::Admin, "Guard: session or token with scope = admin.");
