//! `/meta` — platform metadata for read-scoped clients (`GET /settings`
//! carries Cloudflare secrets and stays admin-only).

use axum::{Json, extract::State};
use serde::Serialize;

use crate::AppState;

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
