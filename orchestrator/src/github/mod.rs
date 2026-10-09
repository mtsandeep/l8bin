//! GitHub API client for the connect-and-deploy integration: repo browsing,
//! workflow-file authoring, Actions secrets/dispatch/runs. Connection
//! credentials live in the `github_connection` singleton row (App or PAT);
//! this module only speaks the GitHub REST API.

pub mod actions;
pub mod auth;
pub mod manifest;
pub mod repos;
pub mod workflow;

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::AppState;

pub const API_BASE: &str = "https://api.github.com";
pub const USER_AGENT: &str = "litebin-orchestrator";

/// Typed errors so routes can surface actionable guidance instead of raw status codes.
#[derive(Debug)]
pub enum GitHubApiError {
    AuthExpired,
    NotFound,
    Forbidden(String),
    RateLimited,
    Api { status: u16, message: String },
    Network(reqwest::Error),
}

impl std::fmt::Display for GitHubApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthExpired => {
                write!(f, "GitHub token is missing, expired, or revoked — reconnect in Settings → GitHub")
            }
            Self::NotFound => write!(f, "repository or resource not found on GitHub"),
            Self::Forbidden(m) => write!(f, "{m}"),
            Self::RateLimited => write!(f, "GitHub rate limit hit — retry shortly"),
            Self::Api { status, message } => write!(f, "GitHub API error ({status}): {message}"),
            Self::Network(e) => write!(f, "GitHub request failed: {e}"),
        }
    }
}

impl std::error::Error for GitHubApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for GitHubApiError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e)
    }
}

impl GitHubApiError {
    /// Map a non-2xx GitHub response body to the most specific variant we can.
    pub fn from_response(status: reqwest::StatusCode, body: &str) -> Self {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| body.chars().take(200).collect());
        match status.as_u16() {
            401 => Self::AuthExpired,
            404 => Self::NotFound,
            403 => {
                if message.contains("rate limit") {
                    Self::RateLimited
                } else {
                    Self::Forbidden(message)
                }
            }
            429 => Self::RateLimited,
            _ => Self::Api { status: status.as_u16(), message },
        }
    }

    pub fn status_code(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::AuthExpired => StatusCode::BAD_GATEWAY, // upstream credential problem, not ours
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Api { status, .. } => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
            Self::Network(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

fn not_connected() -> GitHubApiError {
    GitHubApiError::Api { status: 503, message: "GitHub is not connected — connect it in Settings → GitHub".into() }
}

/// Minimal REST wrapper. One client per request handler; tokens are resolved
/// at construction (PAT directly, installation token via the auth cache).
#[derive(Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    token: String,
}

impl GitHubClient {
    fn pat(token: String) -> Self {
        Self { http: reqwest::Client::new(), token }
    }

    async fn installation(app: &auth::AppCredentials, installation_id: i64) -> Result<Self, GitHubApiError> {
        let token = auth::installation_token(app, installation_id).await?;
        Ok(Self { http: reqwest::Client::new(), token })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{API_BASE}{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
    }

    fn send(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{API_BASE}{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
    }

    async fn json<T: for<'de> Deserialize<'de>>(&self, rb: reqwest::RequestBuilder) -> Result<T, GitHubApiError> {
        let resp = rb.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GitHubApiError::from_response(status, &body));
        }
        Ok(resp.json::<T>().await?)
    }

    /// Fire-and-status (no body): dispatches and secret puts return 201/204.
    async fn empty(&self, rb: reqwest::RequestBuilder) -> Result<(), GitHubApiError> {
        let resp = rb.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GitHubApiError::from_response(status, &body));
        }
        Ok(())
    }
}

/// App credentials when the connection is in app mode.
pub async fn app_credentials(db: &SqlitePool) -> Result<Option<auth::AppCredentials>, GitHubApiError> {
    let row: Option<(String, Option<i64>, Option<String>)> =
        sqlx::query_as("SELECT mode, app_id, private_key_pem FROM github_connection WHERE id = 1")
            .fetch_optional(db)
            .await
            .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?;
    let Some((mode, app_id, pem)) = row else { return Ok(None) };
    if mode != "app" {
        return Ok(None);
    }
    match (app_id, pem.filter(|p| !p.is_empty())) {
        (Some(app_id), Some(private_key_pem)) => Ok(Some(auth::AppCredentials { app_id, private_key_pem })),
        _ => Err(GitHubApiError::AuthExpired),
    }
}

/// Default client for the connection: PAT directly, or the first active
/// installation in app mode (repo browsing without an explicit pick).
pub async fn client_for_connection(db: &SqlitePool) -> Result<GitHubClient, GitHubApiError> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT mode, pat_token FROM github_connection WHERE id = 1")
            .fetch_optional(db)
            .await
            .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?;
    let Some((mode, pat)) = row else { return Err(not_connected()) };
    match (mode.as_str(), pat) {
        ("pat", pat) => {
            let token = pat.filter(|t| !t.is_empty()).ok_or_else(|| GitHubApiError::AuthExpired)?;
            Ok(GitHubClient::pat(token))
        }
        ("app", _) => {
            // Credentials first: a pending manifest flow (no key yet) must read
            // as "reconnect", not as "app not installed".
            let app = app_credentials(db).await?.ok_or_else(|| GitHubApiError::AuthExpired)?;
            let installation: Option<(i64,)> =
                sqlx::query_as("SELECT id FROM github_installations WHERE status = 'active' ORDER BY id LIMIT 1")
                    .fetch_optional(db)
                    .await
                    .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?;
            let Some((installation_id,)) = installation else {
                return Err(GitHubApiError::Api {
                    status: 503,
                    message: "GitHub App is connected but not installed on any account yet".into(),
                });
            };
            GitHubClient::installation(&app, installation_id).await
        }
        (other, _) => Err(GitHubApiError::Api {
            status: 501,
            message: format!("GitHub connection mode '{other}' is not available yet"),
        }),
    }
}

/// Client scoped to a specific installation (app mode repo pick).
pub async fn client_for_installation(db: &SqlitePool, installation_id: i64) -> Result<GitHubClient, GitHubApiError> {
    let app = app_credentials(db).await?.ok_or_else(|| GitHubApiError::Api {
        status: 503,
        message: "GitHub connection is not in GitHub App mode".into(),
    })?;
    GitHubClient::installation(&app, installation_id).await
}

/// Client for a linked repo: installation-scoped when the link carries one,
/// PAT otherwise.
pub async fn client_for_link(db: &SqlitePool, installation_id: Option<i64>) -> Result<GitHubClient, GitHubApiError> {
    match installation_id {
        Some(id) => client_for_installation(db, id).await,
        None => client_for_connection(db).await,
    }
}

/// Convenience wrapper for handlers.
pub async fn client(state: &AppState) -> Result<GitHubClient, GitHubApiError> {
    client_for_connection(&state.db).await
}
