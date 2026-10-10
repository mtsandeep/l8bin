//! GitHub App webhook receiver: HMAC-verified installation lifecycle events.
//! Push auto-deploy needs no webhook (GitHub runs the generated workflow
//! itself); this keeps installations + connection status fresh when the user
//! installs, suspends, or uninstalls the app.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

/// Constant-time-ish comparison of the hex signature against our MAC.
fn signature_matches(secret: &str, raw: &[u8], presented: &str) -> bool {
    let Some(presented) = presented.strip_prefix("sha256=") else { return false };
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(raw);
    let expected = hex::encode(mac.finalize().into_bytes());
    // Length check first so the byte loop can't short-circuit on length.
    expected.len() == presented.len() && expected.bytes().zip(presented.bytes()).all(|(a, b)| a == b)
}

#[derive(serde::Deserialize)]
struct InstallationEvent {
    installation: InstallationPayload,
    action: String,
}

#[derive(serde::Deserialize)]
struct InstallationPayload {
    id: i64,
    account: AccountPayload,
    repository_selection: Option<String>,
}

#[derive(serde::Deserialize)]
struct AccountPayload {
    login: String,
    #[serde(rename = "type")]
    account_type: Option<String>,
    avatar_url: Option<String>,
}

/// POST /github/webhook — public, verified by X-Hub-Signature-256.
pub async fn github_webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    // Each app connection has its own webhook secret — verify against all.
    let secrets: Vec<(i64, String)> = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT id, webhook_secret FROM github_connection WHERE webhook_secret IS NOT NULL",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(id, s)| s.filter(|v| !v.is_empty()).map(|v| (id, v)))
    .collect();
    if secrets.is_empty() {
        return (StatusCode::NOT_FOUND, "no GitHub App webhook configured").into_response();
    }

    let presented = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let connection_id =
        secrets.iter().find(|(_, secret)| signature_matches(secret, &body, presented)).map(|(id, _)| *id);
    let Some(connection_id) = connection_id else {
        tracing::warn!(len = body.len(), "rejected GitHub webhook with bad signature");
        return (StatusCode::UNAUTHORIZED, "bad signature").into_response();
    };

    let event = headers.get("x-github-event").and_then(|v| v.to_str().ok()).unwrap_or("");
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid JSON").into_response(),
    };

    match event {
        "installation" => {
            let Ok(evt) = serde_json::from_value::<InstallationEvent>(payload) else {
                return (StatusCode::OK, "ignored").into_response();
            };
            let now = chrono::Utc::now().timestamp();
            let status = match evt.action.as_str() {
                "deleted" => "deleted",
                "suspended" => "suspended",
                "unsuspended" | "created" | "new_permissions_accepted" => "active",
                _ => return (StatusCode::OK, "ignored").into_response(),
            };
            if let Err(e) = sqlx::query(
                "INSERT INTO github_installations (id, connection_id, account_login, account_type, account_avatar, repository_selection, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    connection_id = excluded.connection_id,
                    account_login = excluded.account_login, account_type = excluded.account_type,
                    account_avatar = excluded.account_avatar, repository_selection = excluded.repository_selection,
                    status = excluded.status, updated_at = excluded.updated_at",
            )
            .bind(evt.installation.id)
            .bind(connection_id)
            .bind(&evt.installation.account.login)
            .bind(evt.installation.account.account_type.clone().unwrap_or_else(|| "User".into()))
            .bind(&evt.installation.account.avatar_url)
            .bind(evt.installation.repository_selection.clone().unwrap_or_else(|| "selected".into()))
            .bind(status)
            .bind(now)
            .bind(now)
            .execute(&state.db)
            .await
            {
                tracing::warn!(error = %e, "failed to record installation event");
            }
            if evt.action == "deleted" {
                let _ = sqlx::query(
                    "UPDATE project_github_config SET installation_id = NULL, updated_at = ? WHERE installation_id = ?",
                )
                .bind(now)
                .bind(evt.installation.id)
                .execute(&state.db)
                .await;
            }
            tracing::info!(installation = evt.installation.id, action = %evt.action, "GitHub installation event");
            (StatusCode::OK, "ok").into_response()
        }
        // Repo add/remove during installs — repo lists are fetched live, so
        // acknowledging is enough.
        "installation_repositories" | "ping" => (StatusCode::OK, "ok").into_response(),
        _ => (StatusCode::OK, "ignored").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn signature_roundtrip_and_rejection() {
        let body = br#"{"installation":{"id":1}}"#;
        assert!(signature_matches("s3cret", body, &sign("s3cret", body)));
        assert!(!signature_matches("wrong", body, &sign("s3cret", body)));
        assert!(!signature_matches("s3cret", body, "sha256=deadbeef"));
        assert!(!signature_matches("s3cret", body, "not-a-sig"));
    }
}
