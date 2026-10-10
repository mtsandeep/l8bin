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

#[derive(Serialize, utoipa::ToSchema)]
pub struct ConnectionSummary {
    pub id: i64,
    pub mode: String,
    /// App slug or PAT login.
    pub label: Option<String>,
    pub status: String,
    pub status_message: Option<String>,
    pub installations: Vec<InstallationSummary>,
}

#[utoipa::path(
    get,
    path = "/github/connections",
    responses(
        (status = 200, description = "All GitHub connections (apps + PATs) with their installations", body = [ConnectionSummary]),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn list_connections(State(state): State<AppState>) -> impl IntoResponse {
    let conns = match github::list_connections(&state.db).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    let installations = load_installations(&state.db).await;
    let out: Vec<ConnectionSummary> = conns
        .into_iter()
        .map(|c| ConnectionSummary {
            id: c.id,
            mode: c.mode.clone(),
            label: c.label.clone().or_else(|| if c.mode == "pat" { c.pat_login.clone() } else { c.app_slug.clone() }),
            status: c.status.clone(),
            status_message: c.status_message.clone(),
            installations: installations.iter().filter(|i| i.connection_id == c.id).cloned().collect(),
        })
        .collect();
    (StatusCode::OK, Json(out)).into_response()
}

/// DB snapshot of installations (per-connection), reused by the list endpoint.
pub(crate) async fn load_installations(db: &sqlx::SqlitePool) -> Vec<InstallationSummary> {
    sqlx::query_as::<_, (i64, i64, String, String, Option<String>, String, String)>(
        "SELECT id, connection_id, account_login, account_type, account_avatar, repository_selection, status
         FROM github_installations ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, connection_id, account_login, account_type, account_avatar, repository_selection, status)| {
        InstallationSummary {
            id,
            connection_id,
            account_login,
            account_type,
            account_avatar,
            repository_selection,
            status,
        }
    })
    .collect()
}

#[utoipa::path(
    get,
    path = "/github/connection",
    responses(
        (status = 200, description = "Aggregate status across all connections", body = ConnectionStatus),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn connection_status(State(state): State<AppState>) -> impl IntoResponse {
    let conns = match github::list_connections(&state.db).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    if conns.is_empty() {
        return Json(ConnectionStatus {
            connected: false,
            mode: None,
            login: None,
            status: None,
            status_message: None,
        })
        .into_response();
    }
    // Credentials must actually exist — a started-but-unfinished manifest flow
    // must not read as connected.
    let active = conns.iter().find(|c| c.has_credentials() && c.status != "disconnected");
    let pending = conns.iter().find(|c| c.status == "pending");
    match (active, pending) {
        (Some(c), _) => Json(ConnectionStatus {
            connected: true,
            mode: Some(c.mode.clone()),
            login: if c.mode == "pat" { c.pat_login.clone() } else { c.app_slug.clone() },
            status: Some(c.status.clone()),
            status_message: c.status_message.clone(),
        }),
        (None, Some(p)) => Json(ConnectionStatus {
            connected: false,
            mode: Some(p.mode.clone()),
            login: None,
            status: Some("pending".into()),
            status_message: None,
        }),
        (None, None) => Json(ConnectionStatus {
            connected: false,
            mode: conns.first().map(|c| c.mode.clone()),
            login: None,
            status: conns.first().map(|c| c.status.clone()),
            status_message: conns.first().and_then(|c| c.status_message.clone()),
        }),
    }
    .into_response()
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
    // One row per GitHub login: reconnecting a known account refreshes it.
    let res = sqlx::query(
        "UPDATE github_connection
         SET pat_token = ?, pat_login = ?, label = ?, status = 'active', status_message = NULL,
             created_by = ?, pending_manifest = NULL, updated_at = ?
         WHERE mode = 'pat' AND pat_login = ?",
    )
    .bind(&token)
    .bind(&user.login)
    .bind(&user.login)
    .bind(&created_by)
    .bind(now)
    .bind(&user.login)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }
    let res = sqlx::query(
        "INSERT INTO github_connection (label, mode, pat_token, pat_login, status, created_by, created_at, updated_at)
         SELECT ?, 'pat', ?, ?, 'active', ?, ?, ?
         WHERE NOT EXISTS (SELECT 1 FROM github_connection WHERE mode = 'pat' AND pat_login = ?)",
    )
    .bind(&user.login)
    .bind(&token)
    .bind(&user.login)
    .bind(&created_by)
    .bind(now)
    .bind(now)
    .bind(&user.login)
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
    path = "/github/connections/{id}",
    params(("id" = i64, Path, description = "Connection ID")),
    responses(
        (status = 204, description = "Connection removed; its link tokens revoked"),
        (status = 404, description = "No such connection"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "github",
    security(("session_auth" = [])),
)]
pub async fn disconnect(
    State(state): State<AppState>,
    axum::extract::Path(conn_id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM github_connection WHERE id = ?")
        .bind(conn_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if exists.is_none() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "no such connection"}))).into_response();
    }

    // Links served by this connection are dead without its credentials:
    // revoke their tokens and drop the link rows (projects keep running).
    if let Err(e) = sqlx::query(
        "DELETE FROM deploy_tokens WHERE id IN (
            SELECT deploy_token_id FROM project_github_config WHERE connection_id = ? AND deploy_token_id IS NOT NULL)",
    )
    .bind(conn_id)
    .execute(&state.db)
    .await
    {
        return db_err(e).into_response();
    }
    let _ =
        sqlx::query("DELETE FROM project_github_config WHERE connection_id = ?").bind(conn_id).execute(&state.db).await;
    let _ =
        sqlx::query("DELETE FROM github_installations WHERE connection_id = ?").bind(conn_id).execute(&state.db).await;
    if let Err(e) = sqlx::query("DELETE FROM github_connection WHERE id = ?").bind(conn_id).execute(&state.db).await {
        return db_err(e).into_response();
    }

    tracing::info!(connection = conn_id, "GitHub connection removed");
    (StatusCode::NO_CONTENT, "").into_response()
}

/// Mark a connection degraded (bad token, uninstalled app). Called by
/// link/run handlers on GitHubApiError::AuthExpired.
pub(crate) async fn mark_degraded(db: &sqlx::SqlitePool, connection_id: Option<i64>, message: &str) {
    let now = chrono::Utc::now().timestamp();
    let res = match connection_id {
        Some(id) => {
            sqlx::query(
                "UPDATE github_connection SET status = 'degraded', status_message = ?, updated_at = ? WHERE id = ?",
            )
            .bind(message)
            .bind(now)
            .bind(id)
            .execute(db)
            .await
        }
        None => {
            sqlx::query("UPDATE github_connection SET status = 'degraded', status_message = ?, updated_at = ?")
                .bind(message)
                .bind(now)
                .execute(db)
                .await
        }
    };
    if let Err(e) = res {
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

    // A name already attached here means GitHub would reject it anyway
    // ("name already in use" on that account) — fail fast with the reason.
    let taken: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM github_connection WHERE mode = 'app' AND label = ? AND status != 'pending'")
            .bind(&name)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    if taken.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!("an app named '{name}' is already connected — GitHub app names are unique per account; pick another name")
            })),
        )
            .into_response();
    }

    // One pending row at a time: retrying an unfinished setup reuses it.
    let pending = serde_json::json!({ "name": name, "created_at": now }).to_string();
    let res = sqlx::query(
        "UPDATE github_connection SET label = ?, pending_manifest = ?, status = 'pending', status_message = NULL, updated_at = ?
         WHERE status = 'pending'",
    )
    .bind(&name)
    .bind(&pending)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }
    let res = sqlx::query(
        "INSERT INTO github_connection (label, mode, pending_manifest, status, created_at, updated_at)
         SELECT ?, 'app', ?, 'pending', ?, ?
         WHERE NOT EXISTS (SELECT 1 FROM github_connection WHERE status = 'pending')",
    )
    .bind(&name)
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
    // The freshest pending manifest row is the one being exchanged.
    let pending: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, pending_manifest FROM github_connection WHERE status = 'pending' ORDER BY updated_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some((pending_id, pending_json)) = pending else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "manifest flow expired — start again from Settings → GitHub"})),
        )
            .into_response();
    };
    let is_fresh = pending_json
        .as_deref()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .and_then(|v| v.get("created_at").and_then(|t| t.as_i64()))
        .is_some_and(|t| chrono::Utc::now().timestamp() - t < 600);
    if !is_fresh {
        // Stale pending row: drop it so it doesn't read as an unfinished setup.
        let _ = sqlx::query("DELETE FROM github_connection WHERE id = ? AND status = 'pending'")
            .bind(pending_id)
            .execute(&state.db)
            .await;
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
                 WHERE mode = 'app' AND app_id IS NOT NULL AND private_key_pem IS NOT NULL
                 ORDER BY updated_at DESC LIMIT 1",
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
                _ => {
                    let _ = sqlx::query("DELETE FROM github_connection WHERE id = ? AND status = 'pending'")
                        .bind(pending_id)
                        .execute(&state.db)
                        .await;
                    (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": "GitHub doesn't recognize this code — it expired or was already used. Start Connect again (your app list is unaffected)."
                        })),
                    )
                        .into_response()
                }
            };
        }
        Err(e) => return gh_error(e).into_response(),
    };

    // Same app re-created (name reuse after delete) replaces its old row;
    // otherwise the pending row becomes the new connection.
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE github_connection SET
            label = ?, app_id = ?, app_slug = ?, client_id = ?, client_secret = ?, private_key_pem = ?,
            webhook_secret = ?, pat_token = NULL, pat_login = NULL, pending_manifest = NULL,
            status = 'active', status_message = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(&app.slug)
    .bind(app.id)
    .bind(&app.slug)
    .bind(&app.client_id)
    .bind(&app.client_secret)
    .bind(&app.pem)
    .bind(&app.webhook_secret)
    .bind(now)
    .bind(pending_id)
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

#[derive(Serialize, utoipa::ToSchema, Clone)]
pub struct InstallationSummary {
    pub id: i64,
    /// Owning app connection.
    pub connection_id: i64,
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
    // Re-attaching a known app refreshes its row; a new App ID is a new
    // connection (apps from several GitHub accounts coexist).
    let res = sqlx::query(
        "UPDATE github_connection
         SET label = ?, app_slug = ?, private_key_pem = ?, webhook_secret = ?,
             status = 'active', status_message = NULL, pending_manifest = NULL, updated_at = ?
         WHERE mode = 'app' AND app_id = ?",
    )
    .bind(&info.slug)
    .bind(&info.slug)
    .bind(&creds.private_key_pem)
    .bind(payload.webhook_secret.as_deref().filter(|s| !s.is_empty()))
    .bind(now)
    .bind(info.id)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        return db_err(e).into_response();
    }
    let res = sqlx::query(
        "INSERT INTO github_connection (label, mode, app_id, app_slug, private_key_pem, webhook_secret, status, created_at, updated_at)
         SELECT ?, 'app', ?, ?, ?, ?, 'active', ?, ?
         WHERE NOT EXISTS (SELECT 1 FROM github_connection WHERE mode = 'app' AND app_id = ?)",
    )
    .bind(&info.slug)
    .bind(info.id)
    .bind(&info.slug)
    .bind(&creds.private_key_pem)
    .bind(payload.webhook_secret.as_deref().filter(|s| !s.is_empty()))
    .bind(now)
    .bind(now)
    .bind(info.id)
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
    let conns = match github::list_connections(&state.db).await {
        Ok(c) => c,
        Err(e) => return gh_error(e).into_response(),
    };
    let apps: Vec<_> = conns.iter().filter_map(|c| c.app_credentials().map(|a| (c.id, a))).collect();
    if apps.is_empty() {
        return (StatusCode::OK, Json(Vec::<InstallationSummary>::new())).into_response();
    }

    let now = chrono::Utc::now().timestamp();
    // Reconciliation: for each connection whose sync succeeded, remember what
    // GitHub reported so rows GitHub no longer lists (uninstalled on their
    // side, no webhook to tell us) can be dropped.
    let mut synced_connections: Vec<i64> = Vec::new();
    let mut seen_installations: Vec<i64> = Vec::new();
    for (conn_id, app) in &apps {
        let installations = match github::auth::list_installations(app).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(connection = conn_id, error = %e, "failed to sync installations");
                continue;
            }
        };
        synced_connections.push(*conn_id);
        for inst in &installations {
            seen_installations.push(inst.id);
            let status = if inst.suspended_by.is_some() { "suspended" } else { "active" };
            let _ = sqlx::query(
                "INSERT INTO github_installations (id, connection_id, account_login, account_type, account_avatar, repository_selection, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    connection_id = excluded.connection_id,
                    account_login = excluded.account_login, account_type = excluded.account_type,
                    account_avatar = excluded.account_avatar, repository_selection = excluded.repository_selection,
                    status = excluded.status, updated_at = excluded.updated_at",
            )
            .bind(inst.id)
            .bind(conn_id)
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
    }

    // Drop stale rows for successfully synced connections. Links pointing at a
    // removed installation degrade to a clear relink error on next use; the
    // projects themselves are untouched.
    if !synced_connections.is_empty() {
        let conn_ph = vec!["?"; synced_connections.len()].join(",");
        // An empty seen-list with a successful sync means the app genuinely
        // has no installations left — delete all its rows, which is correct.
        let (sql, ids): (String, Vec<i64>) = if seen_installations.is_empty() {
            (format!("DELETE FROM github_installations WHERE connection_id IN ({conn_ph})"), synced_connections.clone())
        } else {
            let seen_ph = vec!["?"; seen_installations.len()].join(",");
            (
                format!(
                    "DELETE FROM github_installations WHERE connection_id IN ({conn_ph}) AND id NOT IN ({seen_ph})"
                ),
                synced_connections.iter().chain(seen_installations.iter()).copied().collect(),
            )
        };
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        let _ = q.execute(&state.db).await;
    }

    (StatusCode::OK, Json(load_installations(&state.db).await)).into_response()
}
