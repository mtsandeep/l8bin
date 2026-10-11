//! GitHub REST client for the connect-and-deploy integration.

pub mod actions;
pub mod auth;
pub mod manifest;
pub mod repos;
pub mod workflow;

use serde::Deserialize;
use sqlx::SqlitePool;

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

/// Minimal REST wrapper; the token is resolved at construction.
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

    /// Fire-and-status (no body).
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

/// One GitHub connection row: a GitHub App (per owning account) or a PAT.
#[derive(Debug, Clone)]
pub struct ConnectionRow {
    pub id: i64,
    pub label: Option<String>,
    pub mode: String,
    pub pat_token: Option<String>,
    pub pat_login: Option<String>,
    pub app_id: Option<i64>,
    pub app_slug: Option<String>,
    pub private_key_pem: Option<String>,
    pub status: String,
    pub status_message: Option<String>,
}

impl ConnectionRow {
    /// Real app credentials only when the app flow actually completed.
    pub fn app_credentials(&self) -> Option<auth::AppCredentials> {
        if self.mode != "app" {
            return None;
        }
        match (self.app_id, self.private_key_pem.as_deref().filter(|p| !p.is_empty())) {
            (Some(app_id), Some(private_key_pem)) => {
                Some(auth::AppCredentials { app_id, private_key_pem: private_key_pem.to_string() })
            }
            _ => None,
        }
    }

    /// Credentials exist (a usable PAT or a completed app).
    pub fn has_credentials(&self) -> bool {
        if self.mode == "pat" {
            return self.pat_token.as_deref().is_some_and(|t| !t.is_empty());
        }
        self.app_credentials().is_some()
    }
}

const CONNECTION_COLUMNS: &str =
    "id, label, mode, pat_token, pat_login, app_id, app_slug, private_key_pem, status, status_message";

type ConnectionTuple = (
    i64,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

fn row_from(t: ConnectionTuple) -> ConnectionRow {
    ConnectionRow {
        id: t.0,
        label: t.1,
        mode: t.2,
        pat_token: t.3,
        pat_login: t.4,
        app_id: t.5,
        app_slug: t.6,
        private_key_pem: t.7,
        status: t.8,
        status_message: t.9,
    }
}

pub async fn list_connections(db: &SqlitePool) -> Result<Vec<ConnectionRow>, GitHubApiError> {
    let rows: Vec<ConnectionTuple> =
        sqlx::query_as(&format!("SELECT {CONNECTION_COLUMNS} FROM github_connection ORDER BY id"))
            .fetch_all(db)
            .await
            .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?;
    Ok(rows.into_iter().map(row_from).collect())
}

pub async fn connection_by_id(db: &SqlitePool, id: i64) -> Result<Option<ConnectionRow>, GitHubApiError> {
    let row: Option<ConnectionTuple> =
        sqlx::query_as(&format!("SELECT {CONNECTION_COLUMNS} FROM github_connection WHERE id = ?"))
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?;
    Ok(row.map(row_from))
}

/// PAT directly, or the connection's first active installation.
pub async fn client_for_connection_id(db: &SqlitePool, id: i64) -> Result<GitHubClient, GitHubApiError> {
    let conn = connection_by_id(db, id).await?.ok_or_else(not_connected)?;
    match conn.mode.as_str() {
        "pat" => {
            let token = conn.pat_token.filter(|t| !t.is_empty()).ok_or_else(|| GitHubApiError::AuthExpired)?;
            Ok(GitHubClient::pat(token))
        }
        "app" => {
            let app = conn.app_credentials().ok_or_else(|| GitHubApiError::AuthExpired)?;
            let installation: Option<(i64,)> = sqlx::query_as(
                "SELECT id FROM github_installations WHERE connection_id = ? AND status = 'active' ORDER BY id LIMIT 1",
            )
            .bind(id)
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
        other => Err(GitHubApiError::Api {
            status: 501,
            message: format!("GitHub connection mode '{other}' is not available yet"),
        }),
    }
}

/// Client for an installation, resolved via github_installations.connection_id.
pub async fn client_for_installation(db: &SqlitePool, installation_id: i64) -> Result<GitHubClient, GitHubApiError> {
    let conn_id: Option<i64> = sqlx::query_scalar("SELECT connection_id FROM github_installations WHERE id = ?")
        .bind(installation_id)
        .fetch_optional(db)
        .await
        .map_err(|e| GitHubApiError::Api { status: 500, message: e.to_string() })?
        .flatten();
    let conn_id = conn_id.ok_or_else(|| GitHubApiError::Api {
        status: 503,
        message: "the app was uninstalled from this GitHub account — relink the repo from the deploy dialog".into(),
    })?;
    let app = connection_by_id(db, conn_id)
        .await?
        .and_then(|c| c.app_credentials())
        .ok_or_else(|| GitHubApiError::AuthExpired)?;
    GitHubClient::installation(&app, installation_id).await
}

/// Installation-scoped when the link carries one, else its connection (PAT).
pub async fn client_for_link(
    db: &SqlitePool,
    connection_id: Option<i64>,
    installation_id: Option<i64>,
) -> Result<GitHubClient, GitHubApiError> {
    match installation_id {
        Some(inst_id) => client_for_installation(db, inst_id).await,
        None => match connection_id {
            Some(conn_id) => client_for_connection_id(db, conn_id).await,
            None => Err(not_connected()),
        },
    }
}
