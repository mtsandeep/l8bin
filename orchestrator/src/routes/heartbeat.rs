use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use sqlx::QueryBuilder;
use tracing::{debug, error, info, warn};

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
pub struct HeartbeatPayload {
    pub hosts: Vec<String>,
}

/// Internal endpoint called by agents to report active project hosts.
/// Validates HMAC signature before processing.
pub async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<HeartbeatPayload>,
) -> StatusCode {
    // ── HMAC validation (same pattern as wake_report) ──────────────
    let node_id = match headers.get("X-Agent-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            warn!("heartbeat: missing X-Agent-Id header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let timestamp_str = match headers.get("X-Agent-Timestamp").and_then(|v| v.to_str().ok()) {
        Some(t) => t.to_string(),
        None => {
            warn!("heartbeat: missing X-Agent-Timestamp header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let signature = match headers.get("X-Agent-Signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s.to_string(),
        None => {
            warn!("heartbeat: missing X-Agent-Signature header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    // Parse timestamp and check freshness (5-minute window)
    let ts: i64 = match timestamp_str.parse() {
        Ok(t) => t,
        Err(_) => {
            warn!("heartbeat: invalid timestamp");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let now = chrono::Utc::now().timestamp();
    if (now - ts).unsigned_abs() > 300 {
        warn!(
            node_id = %node_id,
            age_secs = (now - ts).unsigned_abs(),
            "heartbeat: timestamp too old or in future"
        );
        return StatusCode::UNAUTHORIZED;
    }

    // Look up the node's agent_secret from DB
    let secret: Option<String> = match sqlx::query_scalar::<_, Option<String>>(
        "SELECT agent_secret FROM nodes WHERE id = ?",
    )
    .bind(&node_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(Some(s))) => Some(s),
        Ok(_) => None,
        Err(e) => {
            error!(error = %e, "heartbeat: DB error fetching node secret");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let secret = match secret {
        Some(s) => s,
        None => {
            warn!(node_id = %node_id, "heartbeat: node not found or no secret");
            return StatusCode::UNAUTHORIZED;
        }
    };

    // Recompute HMAC: SHA256(secret, "{timestamp}\n{node_id}")
    let message = format!("{}\n{}", timestamp_str, node_id);
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    mac.update(message.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    // Constant-time comparison
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        warn!(node_id = %node_id, "heartbeat: invalid HMAC signature");
        return StatusCode::UNAUTHORIZED;
    }

    // ── Process heartbeat ──────────────────────────────────────────
    if payload.hosts.is_empty() {
        return StatusCode::OK;
    }

    let domain_suffix = format!(".{}", state.config.domain);
    let dashboard_host = format!("{}.{}", state.config.dashboard_subdomain, state.config.domain);
    let poke_host = format!("{}.{}", state.config.poke_subdomain, state.config.domain);

    let mut subdomain_ids: Vec<&str> = Vec::new();
    let mut custom_domains: Vec<&str> = Vec::new();

    for host in &payload.hosts {
        if host == &dashboard_host || host == &poke_host {
            continue;
        }
        if let Some(subdomain) = host.strip_suffix(&domain_suffix) {
            if !subdomain.is_empty() && !subdomain.contains('.') {
                subdomain_ids.push(subdomain);
            } else {
                custom_domains.push(host);
            }
        } else {
            custom_domains.push(host);
        }
    }

    if subdomain_ids.is_empty() && custom_domains.is_empty() {
        return StatusCode::OK;
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
        "UPDATE projects SET last_active_at = "
    );
    qb.push_bind(now);
    qb.push(", updated_at = ");
    qb.push_bind(now);
    qb.push(" WHERE status = 'running' AND auto_stop_enabled = 1 AND (");

    if !subdomain_ids.is_empty() {
        qb.push("id IN (");
        let mut separated = qb.separated(", ");
        for id in &subdomain_ids {
            separated.push_bind(*id);
        }
        qb.push(")");
    }

    if !custom_domains.is_empty() {
        if !subdomain_ids.is_empty() {
            qb.push(" OR ");
        }
        qb.push("custom_domain IN (");
        let mut separated = qb.separated(", ");
        for cd in &custom_domains {
            separated.push_bind(*cd);
        }
        qb.push(")");
    }

    qb.push(")");

    match qb.build().execute(&state.db).await {
        Ok(result) => {
            if result.rows_affected() > 0 {
                info!(
                    rows = result.rows_affected(),
                    hosts = payload.hosts.len(),
                    node_id = %node_id,
                    "heartbeat: updated last_active_at from agent"
                );
            } else {
                debug!(
                    hosts = payload.hosts.len(),
                    node_id = %node_id,
                    "heartbeat: no matching running projects"
                );
            }
        }
        Err(e) => {
            error!(error = %e, "heartbeat: DB update failed");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    StatusCode::OK
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}
