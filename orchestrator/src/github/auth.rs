//! GitHub App auth: RS256 JWT + cached installation tokens (1 h TTL).

use std::sync::OnceLock;

use dashmap::DashMap;
use serde::Deserialize;

use super::{API_BASE, GitHubApiError, USER_AGENT};

/// App credentials from the manifest flow (or manual paste).
#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub app_id: i64,
    pub private_key_pem: String,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Unix seconds at which the token stops being valid.
    expires_at: i64,
}

/// installation id -> live token. Shared across handler tasks.
static INSTALLATION_TOKENS: OnceLock<DashMap<i64, CachedToken>> = OnceLock::new();

fn token_cache() -> &'static DashMap<i64, CachedToken> {
    INSTALLATION_TOKENS.get_or_init(DashMap::new)
}

/// Mint the short-lived App JWT (GitHub's max: 10 min).
fn app_jwt(app: &AppCredentials) -> Result<String, GitHubApiError> {
    let now = (chrono::Utc::now() - chrono::Duration::minutes(1)).timestamp();
    let exp = now + 600;
    let claims = serde_json::json!({ "iat": now, "exp": exp, "iss": app.app_id });
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(app.private_key_pem.as_bytes())
        .map_err(|e| GitHubApiError::Api { status: 502, message: format!("invalid GitHub App private key: {e}") })?;
    jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256), &claims, &key)
        .map_err(|e| GitHubApiError::Api { status: 502, message: format!("failed to sign App JWT: {e}") })
}

#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: String,
}

/// Fetch (or reuse) an installation access token.
pub async fn installation_token(app: &AppCredentials, installation_id: i64) -> Result<String, GitHubApiError> {
    let now = chrono::Utc::now().timestamp();
    // Refresh 10 min early.
    if let Some(entry) = token_cache().get(&installation_id)
        && now < entry.expires_at - 600
    {
        return Ok(entry.token.clone());
    }

    let jwt = app_jwt(app)?;
    let resp = super::http()
        .post(format!("{API_BASE}/app/installations/{installation_id}/access_tokens"))
        .bearer_auth(jwt)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GitHubApiError::from_response(status, &body));
    }
    let tok: InstallationTokenResponse = resp.json().await?;
    let expires_at = chrono::DateTime::parse_from_rfc3339(&tok.expires_at).map(|t| t.timestamp()).unwrap_or(now + 3300);
    token_cache().insert(installation_id, CachedToken { token: tok.token.clone(), expires_at });
    Ok(tok.token)
}

#[derive(Debug, Deserialize)]
pub struct GhInstallation {
    pub id: i64,
    pub account: GhAccount,
    pub repository_selection: Option<String>,
    pub suspended_by: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct GhAccount {
    pub login: String,
    #[serde(rename = "type")]
    pub account_type: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GhAppInfo {
    pub id: i64,
    pub slug: String,
}

/// Fetch the app's own identity (App-JWT auth) — used to validate pasted
/// credentials before storing them.
pub async fn get_app(app: &AppCredentials) -> Result<GhAppInfo, GitHubApiError> {
    let jwt = app_jwt(app)?;
    let resp = super::http()
        .get(format!("{API_BASE}/app"))
        .bearer_auth(jwt)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GitHubApiError::from_response(status, &body));
    }
    Ok(resp.json().await?)
}

/// List installations of the app (App-JWT auth).
pub async fn list_installations(app: &AppCredentials) -> Result<Vec<GhInstallation>, GitHubApiError> {
    let jwt = app_jwt(app)?;
    let resp = super::http()
        .get(format!("{API_BASE}/app/installations?per_page=100"))
        .bearer_auth(jwt)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GitHubApiError::from_response(status, &body));
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JWT minting requires a real RSA key; without one we only verify the
    /// error path is typed (not a panic). Key handling is exercised e2e.
    #[test]
    fn app_jwt_rejects_bad_pem_without_panicking() {
        let app = AppCredentials { app_id: 123, private_key_pem: "not a pem".into() };
        let err = app_jwt(&app).unwrap_err();
        assert!(matches!(err, GitHubApiError::Api { status: 502, .. }), "{err}");
    }
}
