//! GitHub connection lifecycle: status, PAT connect, App manifest flow,
//! installations, disconnect.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::github::{self, GitHubApiError};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub mode: Option<String>,   // "app" | "pat"
    pub login: Option<String>,  // pat_login or app_slug
    pub status: Option<String>, // active | degraded | disconnected
    pub status_message: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatRequest {
    pub token: String,
}

/// Reattach to an existing GitHub App (App ID + private key from the app's
/// settings page). The manifest flow always mints a NEW app; this is the
/// reconnect path after a dashboard-side disconnect.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct AppCredentialsRequest {
    pub app_id: i64,
    /// Full contents of the app's .pem private key file.
    pub private_key_pem: String,
    /// Only needed when the app has a webhook configured.
    pub webhook_secret: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PatConnected {
    pub login: String,
    /// Non-fatal notes (e.g. missing classic scopes on a fine-grained PAT).
    pub warnings: Vec<String>,
}

fn db_err(e: sqlx::Error) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("database error: {e}")})))
}

async fn load_connection(
    state: &AppState,
) -> Result<
    Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    )>,
    (StatusCode, Json<serde_json::Value>),
> {
    sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
        ),
    >(
        "SELECT mode, pat_login, app_slug, status, status_message, pat_token, app_id, private_key_pem
         FROM github_connection WHERE id = 1",
    )
    .fetch_optional(&state.db)
    .await
    .map_err(db_err)
}

#[utoipa::path(
    get,
    path = "/github/connection",
    responses(
        (status = 200, description = "Connection status (connected=false when never configured or a manifest flow is still pending)", body = ConnectionStatus),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn connection_status(State(state): State<AppState>) -> impl IntoResponse {
    match load_connection(&state).await {
        Ok(None) => {
            Json(ConnectionStatus { connected: false, mode: None, login: None, status: None, status_message: None })
                .into_response()
        }
        Ok(Some((mode, pat_login, app_slug, status, status_message, pat_token, app_id, private_key_pem))) => {
            // Credentials must actually exist — a started-but-unfinished
            // manifest flow has mode='app' with no key, and must not read as
            // connected (the user should see the Connect button again).
            let has_creds = match mode.as_str() {
                "pat" => pat_token.is_some_and(|t| !t.is_empty()),
                "app" => app_id.is_some() && private_key_pem.is_some_and(|p| !p.is_empty()),
                _ => false,
            };
            let connected = has_creds && status.as_deref() != Some("disconnected");
            Json(ConnectionStatus {
                connected,
                mode: Some(mode.clone()),
                login: if mode == "pat" { pat_login } else { app_slug },
                status,
                status_message,
            })
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/github/pat",
    request_body = PatRequest,
    responses(
        (status = 200, description = "Connected via PAT", body = PatConnected),
        (status = 400, description = "Token missing scopes"),
        (status = 401, description = "Token rejected by GitHub"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn connect_pat(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<PatRequest>,
) -> impl IntoResponse {
    let token = payload.token.trim().to_string();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "token is required"}))).into_response();
    }

    // Validate against GitHub before storing anything.
    let client = reqwest::Client::new();
    let resp = match client
        .get(format!("{}/user", github::API_BASE))
        .bearer_auth(&token)
        .header("User-Agent", github::USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("GitHub unreachable: {e}")})))
                .into_response();
        }
    };
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "GitHub rejected this token"})))
            .into_response();
    }
    if !resp.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("GitHub returned {}", resp.status())})),
        )
            .into_response();
    }

    #[derive(serde::Deserialize)]
    struct GhUser {
        login: String,
    }
    // Classic PATs advertise scopes; fine-grained ones don't (permissions are
    // checked per-call). Only warn — don't block fine-grained tokens.
    let scopes = resp.headers().get("x-oauth-scopes").and_then(|v| v.to_str().ok()).map(str::to_string);
    let user: GhUser = match resp.json().await {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("unexpected GitHub response: {e}")})),
            )
                .into_response();
        }
    };

    let mut warnings = Vec::new();
    if let Some(scopes) = scopes {
        let have = |needed: &str| scopes.split(',').any(|s| s.trim() == needed);
        for needed in ["repo", "workflow"] {
            if !have(needed) {
                warnings.push(format!(
                    "classic PAT is missing the '{needed}' scope — repo browsing or workflow writes will fail"
                ));
            }
        }
    }

    let now = chrono::Utc::now().timestamp();
    let created_by = auth_session.user.as_ref().map(|u| u.username.clone());
    let res = sqlx::query(
        "INSERT INTO github_connection (id, mode, pat_token, pat_login, status, status_message, created_by, created_at, updated_at)
         VALUES (1, 'pat', ?, ?, 'active', NULL, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            mode = 'pat', pat_token = excluded.pat_token, pat_login = excluded.pat_login,
            status = 'active', status_message = NULL, created_by = excluded.created_by,
            pending_manifest = NULL,
            updated_at = excluded.updated_at",
    )
    .bind(&token)
    .bind(&user.login)
    .bind(&created_by)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }

    tracing::info!(login = %user.login, "GitHub connected via PAT");
    (StatusCode::OK, Json(PatConnected { login: user.login, warnings })).into_response()
}

#[utoipa::path(
    delete,
    path = "/github/connection",
    responses(
        (status = 204, description = "Disconnected; all GitHub link tokens revoked"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn disconnect(State(state): State<AppState>) -> impl IntoResponse {
    let now = chrono::Utc::now().timestamp();

    // Revoke every link token, then clear credentials. Config rows stay so a
    // reconnect can relink without re-picking repos (the old tokens are dead).
    if let Err(e) = sqlx::query(
        "DELETE FROM deploy_tokens WHERE id IN (SELECT deploy_token_id FROM project_github_config WHERE deploy_token_id IS NOT NULL)",
    )
    .execute(&state.db)
    .await
    {
        return db_err(e).into_response();
    }
    if let Err(e) = sqlx::query(
        "UPDATE github_connection SET
            status = 'disconnected', status_message = 'disconnected by user',
            pat_token = NULL, pat_login = NULL,
            private_key_pem = NULL, client_id = NULL, client_secret = NULL, webhook_secret = NULL,
            app_id = NULL, app_slug = NULL, pending_manifest = NULL,
            updated_at = ? WHERE id = 1",
    )
    .bind(now)
    .execute(&state.db)
    .await
    {
        return db_err(e).into_response();
    }
    if let Err(e) = sqlx::query("UPDATE project_github_config SET deploy_token_id = NULL, updated_at = ?")
        .bind(now)
        .execute(&state.db)
        .await
    {
        return db_err(e).into_response();
    }

    tracing::info!("GitHub disconnected");
    (StatusCode::NO_CONTENT, "").into_response()
}

/// Mark the connection degraded (bad token, uninstalled app). Called by
/// link/run handlers on GitHubApiError::AuthExpired.
pub(crate) async fn mark_degraded(state: &AppState, message: &str) {
    let now = chrono::Utc::now().timestamp();
    if let Err(e) =
        sqlx::query("UPDATE github_connection SET status = 'degraded', status_message = ?, updated_at = ? WHERE id = 1")
            .bind(message)
            .bind(now)
            .execute(&state.db)
            .await
    {
        tracing::warn!(error = %e, "failed to mark github connection degraded");
    }
}

/// Shared error → response mapping for GitHub-backed handlers.
pub(crate) fn gh_error(e: GitHubApiError) -> (StatusCode, Json<serde_json::Value>) {
    (e.status_code(), Json(serde_json::json!({"error": e.to_string(), "github_error": e.to_string()})))
}

// ── GitHub App manifest flow ─────────────────────────────────────────────────

#[derive(Serialize, utoipa::ToSchema)]
pub struct ManifestStartResponse {
    /// Raw manifest JSON — the dashboard POSTs it to
    /// github.com/settings/apps/new as the `manifest` form field (GitHub's
    /// documented prefill flow; no URL encoding involved).
    pub manifest: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ManifestExchangeRequest {
    /// Single-use code GitHub appended to the redirect_url after registration.
    pub code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ManifestExchangeResponse {
    pub app_slug: String,
    /// Where the user picks which repos the app may see.
    pub install_url: String,
}

/// Guided-flow input: name (default "l8bin") and an optional webhook override
/// for users who unchecked "automated" and want to point events elsewhere.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct ManifestStartRequest {
    /// GitHub App display name. Default "l8bin". Max 34 chars, letters,
    /// digits, spaces and hyphens (GitHub's rules).
    pub name: Option<String>,
    /// Webhook URL override; omit to use {dashboard}/github/webhook (or no
    /// webhook at all when the dashboard isn't publicly reachable).
    pub webhook_url: Option<String>,
}

/// GitHub display names: up to 34 chars of letters, digits, spaces, hyphens.
fn sanitize_app_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("app name must not be empty".into());
    }
    if name.chars().count() > 34 {
        return Err("app name is limited to 34 characters".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-') {
        return Err("app name may only contain letters, numbers, spaces, and hyphens".into());
    }
    Ok(name.to_string())
}

#[utoipa::path(
    post,
    path = "/github/manifest",
    request_body = ManifestStartRequest,
    responses(
        (status = 200, description = "Pending manifest registered; POST the returned JSON to github.com/settings/apps/new as the `manifest` form field", body = ManifestStartResponse),
        (status = 400, description = "Invalid app name or webhook URL"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn start_manifest(
    State(state): State<AppState>,
    Json(payload): Json<ManifestStartRequest>,
) -> impl IntoResponse {
    let name = match payload.name.as_deref().map(sanitize_app_name).transpose() {
        Ok(n) => n.unwrap_or_else(|| "l8bin".to_string()),
        Err(msg) => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg}))).into_response();
        }
    };
    let webhook_override = match payload.webhook_url.as_deref() {
        None => None,
        Some(url) => {
            let url = url.trim();
            if url.is_empty() {
                None
            } else if url.starts_with("http://") || url.starts_with("https://") {
                Some(url.to_string())
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "webhook URL must start with http:// or https://"})),
                )
                    .into_response();
            }
        }
    };

    let p = state.platform.snapshot();
    let dashboard_url = format!("https://{}.{}", p.dashboard_subdomain, p.domain);
    let manifest = github::manifest::manifest_json(&name, &dashboard_url, webhook_override.as_deref());

    let now = chrono::Utc::now().timestamp();
    let pending = serde_json::json!({ "name": name, "created_at": now }).to_string();
    // 'pending' until the manifest code is exchanged for real credentials; an
    // abandoned flow must not read as connected.
    let res = sqlx::query(
        "INSERT INTO github_connection (id, mode, pat_token, pending_manifest, status, created_at, updated_at)
         VALUES (1, 'app', NULL, ?, 'pending', ?, ?)
         ON CONFLICT(id) DO UPDATE SET mode = 'app', pending_manifest = excluded.pending_manifest,
            status = 'pending', status_message = NULL, updated_at = excluded.updated_at",
    )
    .bind(&pending)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }

    (StatusCode::OK, Json(ManifestStartResponse { manifest: manifest.to_string() })).into_response()
}

#[utoipa::path(
    post,
    path = "/github/manifest/exchange",
    request_body = ManifestExchangeRequest,
    responses(
        (status = 200, description = "App credentials stored; install next", body = ManifestExchangeResponse),
        (status = 400, description = "Code expired or manifest flow not started"),
        (status = 502, description = "GitHub rejected the exchange"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn exchange_manifest(
    State(state): State<AppState>,
    Json(payload): Json<ManifestExchangeRequest>,
) -> impl IntoResponse {
    // Correlate with the manifest we issued (10-minute window).
    let pending: Option<(Option<String>,)> =
        sqlx::query_as("SELECT pending_manifest FROM github_connection WHERE id = 1")
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let is_fresh = pending
        .and_then(|(p,)| p)
        .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
        .and_then(|v| v.get("created_at").and_then(|t| t.as_i64()))
        .is_some_and(|t| chrono::Utc::now().timestamp() - t < 600);
    if !is_fresh {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "manifest flow expired — start again from Settings → GitHub"})),
        )
            .into_response();
    }

    let app = match github::manifest::exchange_code(payload.code.trim()).await {
        Ok(a) => a,
        Err(GitHubApiError::NotFound) => {
            // Manifest codes are single-use. A repeat landing on the callback
            // (refresh, second tab, dev-mode double effect) re-exchanges a
            // consumed code — if the winning exchange already stored the app,
            // report success instead of a confusing 404.
            let existing: Option<(Option<String>,)> = sqlx::query_as(
                "SELECT app_slug FROM github_connection
                 WHERE id = 1 AND mode = 'app' AND app_id IS NOT NULL AND private_key_pem IS NOT NULL",
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
            return match existing {
                Some((Some(slug),)) => {
                    tracing::info!(app = %slug, "GitHub App manifest code replayed — app already connected");
                    (
                        StatusCode::OK,
                        Json(ManifestExchangeResponse {
                            app_slug: slug.clone(),
                            install_url: format!("https://github.com/apps/{slug}/installations/new"),
                        }),
                    )
                        .into_response()
                }
                _ => (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "GitHub doesn't recognize this code — it expired or was already used. Start Connect again (your app list is unaffected)."
                    })),
                )
                    .into_response(),
            };
        }
        Err(e) => return gh_error(e).into_response(),
    };

    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE github_connection SET
            app_id = ?, app_slug = ?, client_id = ?, client_secret = ?, private_key_pem = ?,
            webhook_secret = ?, pat_token = NULL, pat_login = NULL, pending_manifest = NULL,
            status = 'active', status_message = NULL, updated_at = ?
         WHERE id = 1",
    )
    .bind(app.id)
    .bind(&app.slug)
    .bind(&app.client_id)
    .bind(&app.client_secret)
    .bind(&app.pem)
    .bind(&app.webhook_secret)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }

    tracing::info!(app = %app.slug, app_id = app.id, "GitHub App connected via manifest flow");
    (
        StatusCode::OK,
        Json(ManifestExchangeResponse {
            app_slug: app.slug.clone(),
            install_url: format!("https://github.com/apps/{}/installations/new", app.slug),
        }),
    )
        .into_response()
}

// ── Installations (app mode) ─────────────────────────────────────────────────

#[derive(Serialize, utoipa::ToSchema)]
pub struct InstallationSummary {
    pub id: i64,
    pub account_login: String,
    pub account_type: String,
    pub account_avatar: Option<String>,
    pub repository_selection: String,
    pub status: String,
}

#[utoipa::path(
    post,
    path = "/github/app",
    request_body = AppCredentialsRequest,
    responses(
        (status = 200, description = "Existing app attached (credentials validated against GitHub)", body = ManifestExchangeResponse),
        (status = 400, description = "App ID / private key rejected"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn connect_app(
    State(state): State<AppState>,
    Json(payload): Json<AppCredentialsRequest>,
) -> impl IntoResponse {
    let pem = payload.private_key_pem.trim().to_string();
    if payload.app_id <= 0 || pem.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "app_id and private_key_pem are required"})),
        )
            .into_response();
    }

    // Validate the pair against GitHub before storing anything.
    let creds = github::auth::AppCredentials { app_id: payload.app_id, private_key_pem: pem };
    let info = match github::auth::get_app(&creds).await {
        Ok(i) => i,
        Err(GitHubApiError::AuthExpired) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "GitHub rejected this App ID + private key — check both against the app's settings page"})),
            )
                .into_response();
        }
        Err(GitHubApiError::Api { status: 404, .. }) | Err(GitHubApiError::NotFound) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("no GitHub App with ID {} — copy the ID from the app's 'About' section", payload.app_id)})),
            )
                .into_response();
        }
        Err(e) => return gh_error(e).into_response(),
    };

    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "INSERT INTO github_connection
            (id, mode, app_id, app_slug, private_key_pem, webhook_secret, status, created_at, updated_at)
         VALUES (1, 'app', ?, ?, ?, ?, 'active', ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            mode = 'app', app_id = excluded.app_id, app_slug = excluded.app_slug,
            private_key_pem = excluded.private_key_pem, webhook_secret = excluded.webhook_secret,
            pat_token = NULL, pat_login = NULL, pending_manifest = NULL,
            status = 'active', status_message = NULL, updated_at = excluded.updated_at",
    )
    .bind(info.id)
    .bind(&info.slug)
    .bind(&creds.private_key_pem)
    .bind(payload.webhook_secret.as_deref().filter(|s| !s.is_empty()))
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }

    tracing::info!(app = %info.slug, app_id = info.id, "GitHub App attached from pasted credentials");
    (
        StatusCode::OK,
        Json(ManifestExchangeResponse {
            app_slug: info.slug.clone(),
            install_url: format!("https://github.com/apps/{}/installations/new", info.slug),
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/github/installations",
    responses(
        (status = 200, description = "Installations synced from GitHub", body = [InstallationSummary]),
        (status = 404, description = "Not in GitHub App mode"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn list_installations(State(state): State<AppState>) -> impl IntoResponse {
    let Some(app) = github::app_credentials(&state.db).await.ok().flatten() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no GitHub App connection — use the manifest flow or a PAT"})),
        )
            .into_response();
    };

    let installations = match github::auth::list_installations(&app).await {
        Ok(list) => list,
        Err(e) => return gh_error(e).into_response(),
    };

    // Mirror into our table (webhook events keep it fresh between polls).
    let now = chrono::Utc::now().timestamp();
    for inst in &installations {
        let status = if inst.suspended_by.is_some() { "suspended" } else { "active" };
        let _ = sqlx::query(
            "INSERT INTO github_installations (id, account_login, account_type, account_avatar, repository_selection, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                account_login = excluded.account_login, account_type = excluded.account_type,
                account_avatar = excluded.account_avatar, repository_selection = excluded.repository_selection,
                status = excluded.status, updated_at = excluded.updated_at",
        )
        .bind(inst.id)
        .bind(&inst.account.login)
        .bind(inst.account.account_type.clone().unwrap_or_else(|| "User".into()))
        .bind(&inst.account.avatar_url)
        .bind(inst.repository_selection.clone().unwrap_or_else(|| "selected".into()))
        .bind(status)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await;
    }

    let out: Vec<InstallationSummary> = installations
        .into_iter()
        .map(|inst| InstallationSummary {
            id: inst.id,
            account_login: inst.account.login,
            account_type: inst.account.account_type.unwrap_or_else(|| "User".into()),
            account_avatar: inst.account.avatar_url,
            repository_selection: inst.repository_selection.unwrap_or_else(|| "selected".into()),
            status: if inst.suspended_by.is_some() { "suspended".into() } else { "active".into() },
        })
        .collect();
    (StatusCode::OK, Json(out)).into_response()
}
