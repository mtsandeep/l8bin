use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::AppState;
use litebin_common::cloudflare::CloudflareClient;

use super::get_setting;

#[derive(Serialize, utoipa::ToSchema)]
pub struct CleanupDnsResponse {
    pub deleted_count: usize,
}

#[utoipa::path(
    post,
    path = "/settings/cleanup-dns",
    responses(
        (status = 200, body = CleanupDnsResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn cleanup_dns(State(state): State<AppState>) -> Result<Json<CleanupDnsResponse>, (StatusCode, String)> {
    let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
    let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

    if cf_token.is_empty() || cf_zone.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Cloudflare API token and Zone ID must be configured".into()));
    }

    let domain = state.platform.domain();
    let suffix = format!(".{}", domain);

    let cloudflare = CloudflareClient::new(&cf_token, &cf_zone);
    let records = cloudflare
        .list_records_by_suffix(&suffix, "A")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut deleted_count = 0usize;
    for record in &records {
        match cloudflare.delete_record(&record.id).await {
            Ok(_) => deleted_count += 1,
            Err(e) => {
                tracing::warn!(record = %record.name, error = %e, "failed to delete DNS record during cleanup");
            }
        }
    }

    tracing::info!(deleted = deleted_count, total = records.len(), "DNS cleanup complete");
    Ok(Json(CleanupDnsResponse { deleted_count }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SyncDnsResponse {
    pub created: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub errors: usize,
}

#[utoipa::path(
    post,
    path = "/settings/sync-dns",
    responses(
        (status = 200, body = SyncDnsResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn sync_dns(State(state): State<AppState>) -> Result<Json<SyncDnsResponse>, (StatusCode, String)> {
    let cf_token = get_setting(&state.db, "cloudflare_api_token").await?.unwrap_or_default();
    let cf_zone = get_setting(&state.db, "cloudflare_zone_id").await?.unwrap_or_default();

    if cf_token.is_empty() || cf_zone.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Cloudflare API token and Zone ID must be configured".into()));
    }

    let snap = state.platform.snapshot();
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);

    let routes = crate::routing_helpers::resolve_all_routes(&state.db, &snap.domain, &orchestrator_upstream)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let result = state
        .router
        .read()
        .await
        .sync_dns_only(&routes, &snap.domain, &snap.dashboard_subdomain, &snap.poke_subdomain)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SyncDnsResponse {
        created: result.created,
        deleted: result.deleted,
        unchanged: result.unchanged,
        errors: result.errors,
    }))
}
