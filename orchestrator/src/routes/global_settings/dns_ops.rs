use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use litebin_common::cloudflare::CloudflareClient;
use litebin_common::routing::RoutingProvider as _;

use super::get_setting;

#[derive(Serialize, utoipa::ToSchema)]
pub struct CleanupDnsResponse {
    pub deleted_count: usize,
}

#[utoipa::path(
    post,
    path = "/settings/cleanup-dns",
    description = "Delete all DNS records LiteBin created (per its ledger); user-created records are left alone.",
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

    // Delete only records LiteBin created (dns_records ledger) — records the
    // user created themselves are left alone. This covers the whole zone
    // (platform-domain subdomains and project custom domains alike), since
    // Cloudflare integration is being removed entirely.
    let records =
        crate::dns_ledger::rows(&state.db).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let cloudflare = CloudflareClient::new(&cf_token, &cf_zone);

    let mut deleted_count = 0usize;
    for row in &records {
        let zone = if row.zone_id.is_empty() { cf_zone.as_str() } else { row.zone_id.as_str() };
        match cloudflare.delete_record_in_zone(zone, &row.record_id).await {
            Ok(_) => {
                deleted_count += 1;
                let _ = crate::dns_ledger::remove(&state.db, &row.name).await;
            }
            Err(e) => {
                tracing::warn!(record = %row.name, error = %e, "failed to delete DNS record during cleanup");
            }
        }
    }

    tracing::info!(
        deleted = deleted_count,
        total = records.len(),
        "DNS cleanup complete (LiteBin-managed records only)"
    );
    Ok(Json(CleanupDnsResponse { deleted_count }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CloudflareValidateRequest {
    /// Typed credentials; falls back to the saved ones when omitted.
    pub cloudflare_api_token: Option<String>,
    pub cloudflare_zone_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CloudflareValidateResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_name: Option<String>,
    pub errors: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/settings/cloudflare/validate",
    request_body = CloudflareValidateRequest,
    description = "Validate Cloudflare credentials (typed or saved): readable zone covering the platform domain.",
    responses(
        (status = 200, body = CloudflareValidateResponse),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn validate_cloudflare(
    State(state): State<AppState>,
    body: Option<Json<CloudflareValidateRequest>>,
) -> Result<Json<CloudflareValidateResponse>, (StatusCode, String)> {
    let typed = body.map(|Json(b)| b);
    let (typed_token, typed_zone) = match &typed {
        Some(b) => (b.cloudflare_api_token.clone(), b.cloudflare_zone_id.clone()),
        None => (None, None),
    };
    let token = match typed_token.filter(|t| !t.is_empty()) {
        Some(t) => Some(t),
        None => get_setting(&state.db, "cloudflare_api_token").await?.filter(|t| !t.is_empty()),
    };
    let zone = match typed_zone.filter(|z| !z.is_empty()) {
        Some(z) => Some(z),
        None => get_setting(&state.db, "cloudflare_zone_id").await?.filter(|z| !z.is_empty()),
    };

    let mut errors = Vec::new();
    if token.is_none() || zone.is_none() {
        errors.push("API token and Zone ID are required".into());
        return Ok(Json(CloudflareValidateResponse { ok: false, zone_name: None, errors }));
    }

    let cf = CloudflareClient::new(token.as_deref().unwrap(), zone.as_deref().unwrap());
    let mut zone_name = None;
    match cf.get_zone_name().await {
        Ok(name) => {
            let domain = state.platform.domain();
            if !CloudflareClient::zone_covers_hostname(&name, &domain) {
                errors.push(format!("zone '{name}' does not cover '{domain}'"));
            } else if let Err(e) = cf.list_records_by_suffix(&format!(".{domain}"), "A").await {
                errors.push(format!("token cannot list DNS records: {e}"));
            } else {
                zone_name = Some(name);
            }
        }
        Err(_) => errors.push("Cloudflare rejected the request — check the API token and Zone ID".into()),
    }
    Ok(Json(CloudflareValidateResponse { ok: errors.is_empty(), zone_name, errors }))
}

#[derive(Deserialize, Default, utoipa::ToSchema)]
pub struct SyncDnsRequest {
    /// Re-point records whose current IP is unknown to LiteBin (conflicts).
    #[serde(default)]
    pub force: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SyncDnsResponse {
    pub created: usize,
    pub deleted: usize,
    pub repointed: usize,
    pub unchanged: usize,
    pub errors: usize,
    pub skipped: usize,
    pub conflicts: Vec<litebin_common::routing::DnsConflict>,
}

#[utoipa::path(
    post,
    path = "/settings/sync-dns",
    description = "Reconcile Cloudflare DNS records for the platform domain and project custom domains.",
    responses(
        (status = 200, body = SyncDnsResponse),
        (status = 400),
        (status = 500),
    ),
    tag = "global-settings",
    security(("session_auth" = []))
)]
pub async fn sync_dns(
    State(state): State<AppState>,
    body: Option<Json<SyncDnsRequest>>,
) -> Result<Json<SyncDnsResponse>, (StatusCode, String)> {
    let force = body.map(|Json(b)| b.force).unwrap_or(false);
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

    // Reconcile directly from configured creds — independent of routing mode.
    let cloudflare = CloudflareClient::new(&cf_token, &cf_zone);
    let router = crate::cloudflare_router::CloudflareDnsRouter::new(
        cloudflare,
        litebin_common::caddy::CaddyClient::new(&state.config.caddy_admin_url),
        state.node_clients.clone(),
        state.db.clone(),
        state.config.clone(),
    );
    let result = router
        .sync_dns_only(&routes, &snap.domain, &snap.dashboard_subdomain, &snap.poke_subdomain, force)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.starts_with("cloudflare") {
                (StatusCode::BAD_REQUEST, "Cloudflare rejected the request — check the API token and Zone ID".into())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        })?;

    Ok(Json(SyncDnsResponse {
        created: result.created,
        deleted: result.deleted,
        repointed: result.repointed,
        unchanged: result.unchanged,
        errors: result.errors,
        skipped: result.skipped,
        conflicts: result.conflicts,
    }))
}
