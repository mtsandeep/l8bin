//! Split-horizon DNS settings: Dashboard toggle + live status. The reconcile
//! task (orchestrator/src/dns_sidecar.rs) owns the sidecar lifecycle; these
//! endpoints only flip the setting and wake it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::AppState;

#[derive(serde::Serialize)]
pub struct SplitDnsSettingsResponse {
    #[serde(flatten)]
    pub status: crate::dns_sidecar::SplitDnsStatus,
}

pub async fn get_split_dns(
    State(state): State<AppState>,
) -> Result<Json<SplitDnsSettingsResponse>, (StatusCode, String)> {
    let mut status = state.split_dns_status.read().expect("split dns status lock").clone();
    // The stored status lags one reconcile on boot; reflect the setting itself.
    status.enabled = super::get_setting(&state.db, "split_dns_enabled").await?.is_some_and(|v| v == "1");
    Ok(Json(SplitDnsSettingsResponse { status }))
}

#[derive(Deserialize)]
pub struct SplitDnsSettingsRequest {
    pub enabled: bool,
}

pub async fn put_split_dns(
    State(state): State<AppState>,
    Json(body): Json<SplitDnsSettingsRequest>,
) -> Result<Json<SplitDnsSettingsResponse>, (StatusCode, String)> {
    super::upsert_setting(&state.db, "split_dns_enabled", if body.enabled { "1" } else { "0" }).await?;
    // Wake the reconcile loop for an immediate ensure/teardown.
    state.split_dns_notify.notify_one();
    // Give the immediate reconcile a beat to reflect reality.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let status = state.split_dns_status.read().expect("split dns status lock").clone();
    Ok(Json(SplitDnsSettingsResponse { status }))
}
