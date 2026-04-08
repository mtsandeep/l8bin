pub mod backend;

use axum::http::HeaderMap;
use axum_login::AuthManagerLayerBuilder;
use sha2::{Digest, Sha256};
use tower_sessions::SessionManagerLayer;
use tower_sessions::cookie::time;
use tower_sessions::Expiry;
use tower_sessions_sqlx_store::SqliteStore;

use crate::db::models::DeployToken;
use crate::AppState;

pub fn auth_layer(state: AppState) -> axum_login::AuthManagerLayer<backend::PasswordBackend, SqliteStore> {
    let session_store = SqliteStore::new(state.db.clone());
    let secure = std::env::var("COOKIE_SECURE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(secure)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(30)));

    let backend = backend::PasswordBackend::new(state.db.clone());
    AuthManagerLayerBuilder::new(backend, session_layer).build()
}

/// Extract and validate a deploy token from the Authorization header.
/// Returns Some(user_id) if a valid token is found for the given project_id.
/// Returns None if no Bearer token is present or the token is invalid.
pub async fn extract_deploy_token(
    state: &AppState,
    headers: &HeaderMap,
    project_id: &str,
) -> Option<String> {
    let auth_header = headers.get("authorization")?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?.trim();

    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let now = chrono::Utc::now().timestamp();

    let token_row: Option<DeployToken> = sqlx::query_as(
        "SELECT * FROM deploy_tokens WHERE token_hash = ? AND (project_id IS NULL OR project_id = ?) AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(&token_hash)
    .bind(project_id)
    .bind(now)
    .fetch_optional(&state.db)
    .await
    .ok()?;

    let t = token_row?;

    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("UPDATE deploy_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(&t.id)
        .execute(&state.db)
        .await;

    Some(t.user_id)
}
