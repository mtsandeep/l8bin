use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
pub struct WakeReport {
    pub project_id: String,
    pub container_id: String,
    pub mapped_port: u16,
}

/// Internal endpoint called by agents after a successful local wake.
/// Validates HMAC signature from the agent before processing.
pub async fn wake_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<WakeReport>,
) -> StatusCode {
    // ── HMAC validation ──────────────────────────────────────────
    let node_id = match headers.get("X-Agent-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            tracing::warn!("wake report: missing X-Agent-Id header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let timestamp_str = match headers.get("X-Agent-Timestamp").and_then(|v| v.to_str().ok()) {
        Some(t) => t.to_string(),
        None => {
            tracing::warn!("wake report: missing X-Agent-Timestamp header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let signature = match headers.get("X-Agent-Signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s.to_string(),
        None => {
            tracing::warn!("wake report: missing X-Agent-Signature header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    // Parse timestamp and check freshness (5-minute window)
    let ts: i64 = match timestamp_str.parse() {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!("wake report: invalid timestamp");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let now = chrono::Utc::now().timestamp();
    if (now - ts).unsigned_abs() > 300 {
        tracing::warn!(
            node_id = %node_id,
            age_secs = (now - ts).unsigned_abs(),
            "wake report: timestamp too old or in future"
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
        Ok(_) => None, // node not found or agent_secret is NULL
        Err(e) => {
            tracing::error!(error = %e, "wake report: DB error fetching node secret");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let secret = match secret {
        Some(s) => s,
        None => {
            tracing::warn!(node_id = %node_id, "wake report: node not found or no secret");
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

    // Constant-time comparison of hex signatures
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        tracing::warn!(node_id = %node_id, "wake report: invalid HMAC signature");
        return StatusCode::UNAUTHORIZED;
    }

    // ── Process wake report ──────────────────────────────────────
    tracing::info!(
        project_id = %report.project_id,
        container_id = %report.container_id,
        mapped_port = %report.mapped_port,
        node_id = %node_id,
        "received wake report from agent"
    );

    let result = sqlx::query(
        "UPDATE projects SET status = 'running', container_id = ?, mapped_port = ?, last_active_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&report.container_id)
    .bind(report.mapped_port as i64)
    .bind(now)
    .bind(now)
    .bind(&report.project_id)
    .execute(&state.db)
    .await;

    match result {
        Ok(r) => {
            if r.rows_affected() == 0 {
                tracing::warn!(project_id = %report.project_id, "wake report: project not found in DB");
                return StatusCode::NOT_FOUND;
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "wake report: DB update failed");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    // Trigger debounced route sync
    let _ = state.route_sync_tx.send(());

    StatusCode::OK
}

/// Constant-time byte comparison to prevent timing attacks.
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
