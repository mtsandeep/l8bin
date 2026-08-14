//! Shared HMAC request signing between the orchestrator and agents.
//!
//! Agents sign internal requests (wake reports, heartbeats) with the node's
//! `agent_secret`; the orchestrator recomputes and verifies the signature.
//! Message format: `"{timestamp}\n{node_id}"`, HMAC-SHA256, hex-encoded.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const AGENT_ID_HEADER: &str = "X-Agent-Id";
pub const AGENT_TIMESTAMP_HEADER: &str = "X-Agent-Timestamp";
pub const AGENT_SIGNATURE_HEADER: &str = "X-Agent-Signature";

/// How far from "now" a signed timestamp may be (seconds) before rejection.
pub const SIGNATURE_FRESHNESS_SECS: i64 = 300;

/// Compute the hex HMAC-SHA256 signature for an agent request.
pub fn sign_agent_request(secret: &str, node_id: &str, timestamp: i64) -> Option<String> {
    let message = format!("{}\n{}", timestamp, node_id);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Why a signature verification failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The timestamp header is not a valid integer.
    InvalidTimestamp,
    /// The timestamp is outside the freshness window; carries its age in seconds.
    StaleTimestamp(u64),
    /// The recomputed signature does not match the provided one.
    SignatureMismatch,
}

/// Verify an agent request signature (freshness window + constant-time compare).
pub fn verify_agent_signature(
    secret: &str,
    node_id: &str,
    timestamp_str: &str,
    signature: &str,
) -> Result<(), VerifyError> {
    let ts: i64 = timestamp_str.parse().map_err(|_| VerifyError::InvalidTimestamp)?;

    let age = (chrono::Utc::now().timestamp() - ts).unsigned_abs();
    if age > SIGNATURE_FRESHNESS_SECS as u64 {
        return Err(VerifyError::StaleTimestamp(age));
    }

    // Recompute HMAC: SHA256(secret, "{timestamp}\n{node_id}")
    let message = format!("{}\n{}", timestamp_str, node_id);
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return Err(VerifyError::SignatureMismatch),
    };
    mac.update(message.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err(VerifyError::SignatureMismatch);
    }
    Ok(())
}

/// Constant-time byte comparison to prevent timing attacks.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_request_round_trips() {
        let ts = chrono::Utc::now().timestamp();
        let sig = sign_agent_request("secret", "node-1", ts).unwrap();
        assert!(verify_agent_signature("secret", "node-1", &ts.to_string(), &sig).is_ok());
    }

    #[test]
    fn wrong_secret_or_node_is_rejected() {
        let ts = chrono::Utc::now().timestamp();
        let sig = sign_agent_request("secret", "node-1", ts).unwrap();
        assert_eq!(
            verify_agent_signature("other", "node-1", &ts.to_string(), &sig),
            Err(VerifyError::SignatureMismatch)
        );
        assert_eq!(
            verify_agent_signature("secret", "node-2", &ts.to_string(), &sig),
            Err(VerifyError::SignatureMismatch)
        );
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let old = (chrono::Utc::now().timestamp() - SIGNATURE_FRESHNESS_SECS - 10).to_string();
        let sig = sign_agent_request("secret", "node-1", 0).unwrap();
        assert!(matches!(verify_agent_signature("secret", "node-1", &old, &sig), Err(VerifyError::StaleTimestamp(_))));
    }

    #[test]
    fn invalid_timestamp_is_rejected() {
        let sig = sign_agent_request("secret", "node-1", 0).unwrap();
        assert_eq!(
            verify_agent_signature("secret", "node-1", "not-a-number", &sig),
            Err(VerifyError::InvalidTimestamp)
        );
    }
}
