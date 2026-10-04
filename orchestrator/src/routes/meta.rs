//! `/meta` — platform metadata for read-scoped clients (`GET /settings`
//! carries Cloudflare secrets and stays admin-only).
//! `/whoami` — which credential (token or session) is making this request.

use axum::{Extension, Json, extract::State};
use axum_login::AuthSession;
use serde::Serialize;
use serde_json::json;

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::auth::guard::TokenContext;

#[derive(Serialize, utoipa::ToSchema)]
pub struct MetaResponse {
    /// Platform domain used for project URLs: `https://{project_id}.{domain}`.
    pub domain: String,
    pub dashboard_subdomain: String,
    pub poke_subdomain: String,
    /// `master_proxy` or `cloudflare_dns`.
    pub routing_mode: String,
    /// Orchestrator version.
    pub version: String,
}

#[utoipa::path(
    get,
    path = "/meta",
    responses(
        (status = 200, description = "Platform metadata", body = MetaResponse),
    ),
    tag = "health",
    security(
        ("session_auth" = []),
        ("bearer_token" = []),
    ),
)]
pub async fn get_meta(State(state): State<AppState>) -> Json<MetaResponse> {
    let routing_mode = crate::routes::global_settings::get_setting(&state.db, "routing_mode")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "master_proxy".to_string());

    Json(MetaResponse {
        domain: state.platform.domain(),
        dashboard_subdomain: state.platform.dashboard_subdomain(),
        poke_subdomain: state.platform.poke_subdomain(),
        routing_mode,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

#[utoipa::path(
    get,
    path = "/whoami",
    responses(
        (status = 200, description = "The credential making this request: token identity (token_id, name, scope, project_id) or session username"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "health",
    security(
        ("session_auth" = []),
        ("bearer_token" = []),
    ),
)]
pub async fn whoami(
    auth_session: AuthSession<PasswordBackend>,
    token: Option<Extension<TokenContext>>,
) -> Json<serde_json::Value> {
    if let Some(user) = auth_session.user {
        return Json(json!({ "kind": "session", "username": user.username }));
    }
    let t = token.expect("authenticated requests carry a session or a token context");
    Json(json!({
        "kind": "token",
        "token_id": t.token_id,
        "name": t.token_name,
        "scope": t.scope,
        "project_id": t.project_id,
    }))
}
