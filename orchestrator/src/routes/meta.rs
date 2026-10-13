//! `/meta` — platform metadata for read-scoped clients (`GET /settings`
//! carries Cloudflare secrets and stays admin-only).
//! `/whoami` — which credential (token or session) is making this request.

use axum::{Extension, Json, extract::State};
use axum_login::AuthSession;
use serde::Serialize;

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::auth::guard::TokenContext;
use litebin_common::types::TokenScope;

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

/// The active credential. `kind` is `session` (only `username` set) or
/// `token` (only the token fields set).
#[derive(Serialize, utoipa::ToSchema)]
pub struct WhoamiResponse {
    /// `session` or `token`.
    pub kind: String,
    /// Session credentials: the logged-in username.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Token credentials: deploy token ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// Token credentials: deploy token display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Token credentials: cumulative access scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TokenScope>,
    /// Token credentials: project binding; absent when global.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/meta",
    description = "Platform metadata for read-scoped clients: domain, routing mode, version.",
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
    description = "Identify the credential making this request: session username or token identity (id, name, scope, project binding).",
    responses(
        (status = 200, description = "Credential identity", body = WhoamiResponse),
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
) -> Json<WhoamiResponse> {
    if let Some(user) = auth_session.user {
        return Json(WhoamiResponse {
            kind: "session".to_string(),
            username: Some(user.username),
            token_id: None,
            name: None,
            scope: None,
            project_id: None,
        });
    }
    let TokenContext { token_id, token_name, scope, project_id, .. } =
        token.expect("authenticated requests carry a session or a token context").0;
    Json(WhoamiResponse {
        kind: "token".to_string(),
        username: None,
        token_id: Some(token_id),
        name: token_name,
        scope: Some(scope),
        project_id,
    })
}
