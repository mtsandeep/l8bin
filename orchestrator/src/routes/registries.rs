//! Managed image registries: CRUD + connectivity test. Every mutation
//! refreshes the DockerManager pull state used for local pulls; agent
//! dispatches receive the state via `nodes::registry_auth::registry_config_for_node`.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::db::models::{Registry, RegistryResponse};
use litebin_common::registry::{RegistryConfig, RegistryWireConfig};

// ── Pull-state loading ────────────────────────────────────────────────────────

/// Full registry pull config (default registry, credentials, mirror chains).
pub async fn load_registry_wire_config(db: &sqlx::SqlitePool) -> anyhow::Result<RegistryWireConfig> {
    let rows: Vec<Registry> = sqlx::query_as("SELECT * FROM registries").fetch_all(db).await?;
    let default = rows.iter().find(|row| row.is_default).map(|row| row.url.clone());
    Ok(RegistryWireConfig {
        default,
        registries: rows
            .into_iter()
            .map(|row| {
                let mirrors = row.mirror_list();
                RegistryConfig { serveraddress: row.url, auth: row.auth_b64.filter(|auth| !auth.is_empty()), mirrors }
            })
            .collect(),
    })
}

/// Push the current registry pull state into the DockerManager used for local
/// pulls. Called at startup and after every mutating endpoint here.
pub async fn refresh_registry_state(
    db: &sqlx::SqlitePool,
    docker: &litebin_common::docker::DockerManager,
) -> anyhow::Result<()> {
    docker.set_registry_state(&load_registry_wire_config(db).await?);
    Ok(())
}

// ── Request/response bodies ───────────────────────────────────────────────────

/// A mirror as sent by clients: endpoint plus credentials as username/password
/// (preferred) or pre-encoded base64 `auth_b64`. On update, omitted
/// credentials keep the stored ones for the same URL.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct MirrorInput {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub auth_b64: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateRegistryRequest {
    pub name: String,
    /// Canonical auth serveraddress, e.g. `https://registry.example.com`.
    pub url: String,
    /// Credentials as the provider hands them out. Preferred over `auth_b64`.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Pre-encoded base64 `user:password`. Omit all credential fields for an
    /// anonymous registry entry.
    #[serde(default)]
    pub auth_b64: Option<String>,
    /// Mirrors tried in order before the registry itself.
    #[serde(default)]
    pub mirrors: Option<Vec<MirrorInput>>,
    /// Make this the registry for image names without a registry host.
    #[serde(default)]
    pub is_default: Option<bool>,
    /// Make this the default push target for `l8b build --push` / `l8b deploy`.
    #[serde(default)]
    pub is_push_default: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateRegistryRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    /// New credentials as username/password (preferred). Omit to keep the
    /// existing credentials.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// New pre-encoded base64 `user:password`. Omit to keep the existing
    /// credentials.
    #[serde(default)]
    pub auth_b64: Option<String>,
    /// Explicitly clear stored credentials (takes precedence over the above).
    #[serde(default)]
    pub clear_auth: bool,
    /// Replace the mirror list. Mirrors whose URL is unchanged and whose
    /// credentials are omitted keep their stored credentials.
    #[serde(default)]
    pub mirrors: Option<Vec<MirrorInput>>,
    /// Make (Some(true)) or drop (Some(false)) this registry as the default
    /// for image names without a registry host. Setting a new default unsets
    /// the previous one; dropping it leaves no default (bare names then use
    /// the daemon's docker.io).
    #[serde(default)]
    pub is_default: Option<bool>,
    /// Make (Some(true)) or drop (Some(false)) this registry as the default
    /// push target for `l8b build --push` / `l8b deploy`.
    #[serde(default)]
    pub is_push_default: Option<bool>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegistryTestResponse {
    /// `ok` (200, auth accepted), `unauthorized` (reachable, credentials
    /// rejected), `unexpected_status` (reachable, odd reply), `unreachable`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub message: String,
}

fn unauthenticated(auth_session: &AuthSession<PasswordBackend>) -> Option<axum::response::Response> {
    auth_session
        .user
        .is_none()
        .then(|| (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response())
}

fn validate_url(url: &str) -> Result<(), String> {
    if litebin_common::registry::host_from_url(url).is_none() {
        Err("url must include a registry host, e.g. https://registry.example.com".to_string())
    } else {
        Ok(())
    }
}

/// Resolve credential input to stored base64 `user:password`. Username +
/// password (the shape providers hand out) is preferred and encoded here;
/// pre-encoded `auth_b64` still works. None = no credentials supplied.
fn resolve_auth_input(
    username: Option<&str>,
    password: Option<&str>,
    auth_b64: Option<&str>,
) -> Result<Option<String>, String> {
    let username = username.map(str::trim).filter(|u| !u.is_empty());
    match (username, password) {
        (Some(username), Some(password)) => {
            use base64::Engine as _;
            Ok(Some(base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))))
        }
        (Some(_), None) | (None, Some(_)) => Err("username and password must be provided together".to_string()),
        (None, None) => Ok(auth_b64.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string)),
    }
}

/// Validate a mirror list and serialize it for storage. Mirrors must serve the
/// upstream registry's repository paths (pull-through caches do).
fn validate_mirrors(mirrors: &[MirrorInput]) -> Result<Vec<litebin_common::registry::MirrorConfig>, String> {
    let mut configs = Vec::with_capacity(mirrors.len());
    for mirror in mirrors {
        if litebin_common::registry::host_from_url(&mirror.url).is_none() {
            return Err(format!("mirror '{}' must include a host, e.g. https://mirror.example.com", mirror.url));
        }
        let auth =
            resolve_auth_input(mirror.username.as_deref(), mirror.password.as_deref(), mirror.auth_b64.as_deref())?;
        configs.push(litebin_common::registry::MirrorConfig { url: mirror.url.trim().to_string(), auth });
    }
    Ok(configs)
}

fn serialize_mirrors(mirrors: &[litebin_common::registry::MirrorConfig]) -> String {
    serde_json::to_string(mirrors).expect("mirror configs always serialize")
}

/// Exactly one registry may be the default; setting one unsets the rest.
async fn unset_other_defaults(db: &sqlx::SqlitePool, keep_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE registries SET is_default = 0 WHERE id != ?").bind(keep_id).execute(db).await.map(|_| ())
}

async fn unset_other_push_defaults(db: &sqlx::SqlitePool, keep_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE registries SET is_push_default = 0 WHERE id != ?").bind(keep_id).execute(db).await.map(|_| ())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/registries",
    description = "List configured image registries. Credentials are masked (has_auth + last characters).",
    responses(
        (status = 200, description = "Registries", body = [RegistryResponse]),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "registries",
    security(("session_auth" = [])),
)]
pub async fn list_registries(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(resp) = unauthenticated(&auth_session) {
        return resp;
    }
    match sqlx::query_as::<_, Registry>("SELECT * FROM registries ORDER BY is_builtin DESC, name ASC")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => {
            (StatusCode::OK, Json(rows.into_iter().map(RegistryResponse::from).collect::<Vec<_>>())).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")}))).into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/registries",
    request_body = CreateRegistryRequest,
    description = "Add a registry (private/authenticated or anonymous).",
    responses(
        (status = 201, description = "Registry created", body = RegistryResponse),
        (status = 400, description = "Invalid name/url or duplicate"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "registries",
    security(("session_auth" = [])),
)]
pub async fn create_registry(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(payload): Json<CreateRegistryRequest>,
) -> impl IntoResponse {
    if let Some(resp) = unauthenticated(&auth_session) {
        return resp;
    }
    let name = payload.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "name is required"}))).into_response();
    }
    if let Err(e) = validate_url(&payload.url) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    let auth_b64 =
        match resolve_auth_input(payload.username.as_deref(), payload.password.as_deref(), payload.auth_b64.as_deref())
        {
            Ok(auth) => auth,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        };
    let mirror_configs = match payload.mirrors.as_deref() {
        Some(list) => match validate_mirrors(list) {
            Ok(configs) => Some(configs),
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        },
        None => None,
    };
    let is_default = payload.is_default.unwrap_or(false);
    let is_push_default = payload.is_push_default.unwrap_or(false);

    let now = chrono::Utc::now().timestamp();
    let registry = Registry {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        url: payload.url.trim().to_string(),
        auth_b64,
        mirrors: mirror_configs.as_deref().map(serialize_mirrors),
        is_default,
        is_push_default,
        is_builtin: false,
        created_at: now,
        updated_at: now,
    };
    if is_default && let Err(e) = unset_other_defaults(&state.db, &registry.id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("failed to set default: {e}")})))
            .into_response();
    }
    if is_push_default && let Err(e) = unset_other_push_defaults(&state.db, &registry.id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("failed to set push default: {e}")})))
            .into_response();
    }
    match sqlx::query("INSERT INTO registries (id, name, url, auth_b64, mirrors, is_default, is_push_default, is_builtin, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?)")
        .bind(&registry.id)
        .bind(&registry.name)
        .bind(&registry.url)
        .bind(&registry.auth_b64)
        .bind(&registry.mirrors)
        .bind(registry.is_default)
        .bind(registry.is_push_default)
        .bind(registry.created_at)
        .bind(registry.updated_at)
        .execute(&state.db)
        .await
    {
        Ok(_) => {}
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("failed to create registry: {e}")})))
                .into_response();
        }
    }
    if let Err(e) = refresh_registry_state(&state.db, &state.docker).await {
        tracing::warn!(error = %e, "failed to refresh registry state after create");
    }
    (StatusCode::CREATED, Json(RegistryResponse::from(registry))).into_response()
}

#[utoipa::path(
    put,
    path = "/registries/{id}",
    request_body = UpdateRegistryRequest,
    description = "Update a registry. Omitted fields are kept; clear_auth removes stored credentials.",
    responses(
        (status = 200, description = "Registry updated", body = RegistryResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Registry not found"),
    ),
    tag = "registries",
    security(("session_auth" = [])),
)]
pub async fn update_registry(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateRegistryRequest>,
) -> impl IntoResponse {
    if let Some(resp) = unauthenticated(&auth_session) {
        return resp;
    }
    let mut registry = match sqlx::query_as::<_, Registry>("SELECT * FROM registries WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "registry not found"}))).into_response(),
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response();
        }
    };

    if let Some(name) = payload.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "name cannot be empty"}))).into_response();
        }
        registry.name = name.to_string();
    }
    if let Some(url) = payload.url.as_deref() {
        if let Err(e) = validate_url(url) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
        }
        registry.url = url.trim().to_string();
    }
    if payload.clear_auth {
        registry.auth_b64 = None;
    } else if let Some(resolved) =
        match resolve_auth_input(payload.username.as_deref(), payload.password.as_deref(), payload.auth_b64.as_deref())
        {
            Ok(resolved) => resolved,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        }
    {
        registry.auth_b64 = Some(resolved);
    }
    if let Some(list) = payload.mirrors.as_deref() {
        let mut configs = match validate_mirrors(list) {
            Ok(configs) => configs,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        };
        // An omitted auth_b64 keeps the stored credentials for the same URL.
        let existing = registry.mirror_list();
        for config in &mut configs {
            if config.auth.is_none()
                && let Some(kept) = existing.iter().find(|m| m.url == config.url)
            {
                config.auth.clone_from(&kept.auth);
            }
        }
        registry.mirrors = Some(serialize_mirrors(&configs));
    }
    if payload.is_default == Some(true) {
        registry.is_default = true;
        if let Err(e) = unset_other_defaults(&state.db, &id).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("failed to set default: {e}")})))
                .into_response();
        }
    } else if payload.is_default == Some(false) {
        registry.is_default = false;
    }
    if payload.is_push_default == Some(true) {
        registry.is_push_default = true;
        if let Err(e) = unset_other_push_defaults(&state.db, &id).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to set push default: {e}")})),
            )
                .into_response();
        }
    } else if payload.is_push_default == Some(false) {
        registry.is_push_default = false;
    }
    registry.updated_at = chrono::Utc::now().timestamp();

    match sqlx::query("UPDATE registries SET name = ?, url = ?, auth_b64 = ?, mirrors = ?, is_default = ?, is_push_default = ?, updated_at = ? WHERE id = ?")
        .bind(&registry.name)
        .bind(&registry.url)
        .bind(&registry.auth_b64)
        .bind(&registry.mirrors)
        .bind(registry.is_default)
        .bind(registry.is_push_default)
        .bind(registry.updated_at)
        .bind(&id)
        .execute(&state.db)
        .await
    {
        Ok(_) => {}
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("failed to update registry: {e}")})))
                .into_response();
        }
    }
    if let Err(e) = refresh_registry_state(&state.db, &state.docker).await {
        tracing::warn!(error = %e, "failed to refresh registry state after update");
    }
    (StatusCode::OK, Json(RegistryResponse::from(registry))).into_response()
}

#[utoipa::path(
    delete,
    path = "/registries/{id}",
    description = "Delete a user-added registry. Built-in registries cannot be deleted.",
    responses(
        (status = 200, description = "Registry deleted"),
        (status = 400, description = "Built-in registries cannot be deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Registry not found"),
    ),
    tag = "registries",
    security(("session_auth" = [])),
)]
pub async fn delete_registry(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(resp) = unauthenticated(&auth_session) {
        return resp;
    }
    let registry = match sqlx::query_as::<_, Registry>("SELECT * FROM registries WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "registry not found"}))).into_response(),
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response();
        }
    };
    if registry.is_builtin {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "built-in registries cannot be deleted"})))
            .into_response();
    }
    match sqlx::query("DELETE FROM registries WHERE id = ?").bind(&id).execute(&state.db).await {
        Ok(_) => {}
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response();
        }
    }
    if let Err(e) = refresh_registry_state(&state.db, &state.docker).await {
        tracing::warn!(error = %e, "failed to refresh registry state after delete");
    }
    (StatusCode::OK, Json(json!({"deleted": id}))).into_response()
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PushDefaultResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Canonical registry host (e.g. `zot.litebin.dev` or `localhost:5010`).
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub insecure: bool,
}

fn decode_auth_b64(auth_b64: Option<&str>) -> (Option<String>, Option<String>) {
    match auth_b64.and_then(litebin_common::registry::decode_auth) {
        Some((u, p)) => (Some(u), Some(p)),
        None => (None, None),
    }
}

#[utoipa::path(
    get,
    path = "/registries/push-default",
    description = "Return the default push registry (if configured) for CLI builds/deploys.",
    responses(
        (status = 200, description = "Default push registry", body = PushDefaultResponse),
        (status = 204, description = "No default push registry configured"),
        (status = 401, description = "Unauthorized"),
    ),
    tag = "registries",
    security(("session_auth" = []), ("bearer_auth" = [])),
)]
pub async fn get_push_default(
    auth_session: AuthSession<PasswordBackend>,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if auth_session.user.is_none() && crate::auth::extract_deploy_token(&state, &headers, "").await.is_none() {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Authentication required"}))).into_response();
    }
    match sqlx::query_as::<_, Registry>("SELECT * FROM registries WHERE is_push_default = 1 LIMIT 1")
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(r)) => {
            let host = litebin_common::registry::host_from_url(&r.url).unwrap_or_else(|| r.url.clone());
            let insecure = r.url.starts_with("http://");
            let (username, password) = decode_auth_b64(r.auth_b64.as_deref());
            (
                StatusCode::OK,
                Json(PushDefaultResponse { id: r.id, name: r.name, url: r.url, host, username, password, insecure }),
            )
                .into_response()
        }
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")}))).into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/registries/{id}/test",
    description = "Connectivity/credential check: GET {url}/v2/ (the OCI registry probe endpoint).",
    responses(
        (status = 200, description = "Test completed", body = RegistryTestResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Registry not found"),
    ),
    tag = "registries",
    security(("session_auth" = [])),
)]
pub async fn test_registry(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(resp) = unauthenticated(&auth_session) {
        return resp;
    }
    let registry = match sqlx::query_as::<_, Registry>("SELECT * FROM registries WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "registry not found"}))).into_response(),
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response();
        }
    };

    // The OCI ping endpoint is at the host root — the stored `url` may carry
    // Docker's legacy /v1/ auth path (https://index.docker.io/v1/).
    let scheme = if registry.url.starts_with("http://") { "http" } else { "https" };
    let Some(host) = litebin_common::registry::host_from_url(&registry.url) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "registry url has no host"}))).into_response();
    };
    let ping_url = format!("{scheme}://{host}/v2/");
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build() {
        Ok(client) => client,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("client build failed: {e}")})))
                .into_response();
        }
    };
    let stored_auth = registry.auth_b64.as_deref().filter(|a| !a.is_empty());
    let mut request = client.get(&ping_url);
    if let Some(auth) = stored_auth {
        request = request.header("Authorization", format!("Basic {auth}"));
    }
    match request.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Token-auth registries (Docker Hub, ghcr.io, quay…) answer 401 with
            // a Bearer challenge even for valid Basic credentials. With stored
            // credentials, verify at the token endpoint the way `docker login`
            // does. Anonymous, there is nothing to verify — public/private is
            // per-package — so a Bearer-challenged ping just means reachable.
            if status == 401
                && let Some(challenge) = resp.headers().get("www-authenticate").and_then(|value| value.to_str().ok())
                && challenge.to_ascii_lowercase().contains("bearer")
            {
                let mut result = match stored_auth {
                    Some(auth) => token_auth_check(&client, challenge, auth, &ping_url).await,
                    None => RegistryTestResponse {
                        status: "ok".into(),
                        http_status: Some(status),
                        message: "registry reachable — public images pull anonymously; private ones need credentials (none stored)".into(),
                    },
                };
                if result.status == "ok" && registry.is_push_default {
                    result = push_probe(&client, scheme, &host, stored_auth).await;
                }
                return (StatusCode::OK, Json(result)).into_response();
            }
            let mut result = match status {
                200 => RegistryTestResponse {
                    status: "ok".into(),
                    http_status: Some(status),
                    message: "registry reachable, credentials accepted".into(),
                },
                401 | 403 => RegistryTestResponse {
                    status: "unauthorized".into(),
                    http_status: Some(status),
                    message: if stored_auth.is_some() {
                        "registry reachable, but the stored credentials were rejected".into()
                    } else {
                        "registry reachable, but it requires credentials (none stored)".into()
                    },
                },
                other => RegistryTestResponse {
                    status: "unexpected_status".into(),
                    http_status: Some(other),
                    message: format!("registry answered with HTTP {other} for /v2/ — verify the URL"),
                },
            };
            if result.status == "ok" && registry.is_push_default {
                result = push_probe(&client, scheme, &host, stored_auth).await;
            }
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(e) => (
            StatusCode::OK,
            Json(RegistryTestResponse {
                status: "unreachable".into(),
                http_status: None,
                message: format!("could not reach {ping_url}: {e}"),
            }),
        )
            .into_response(),
    }
}

/// Verify push permission by initiating (and cancelling) an empty blob upload.
async fn push_probe(
    client: &reqwest::Client,
    scheme: &str,
    host: &str,
    stored_auth: Option<&str>,
) -> RegistryTestResponse {
    let upload_url = format!("{scheme}://{host}/v2/litebin-probe/blobs/uploads/");
    let mut req = client.post(&upload_url);
    if let Some(auth) = stored_auth {
        req = req.header("Authorization", format!("Basic {auth}"));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return RegistryTestResponse {
                status: "unreachable".into(),
                http_status: None,
                message: format!("push probe failed reaching {upload_url}: {e}"),
            };
        }
    };
    let status = resp.status().as_u16();
    if status == 401
        && let Some(challenge) = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok())
        && challenge.to_ascii_lowercase().contains("bearer")
    {
        let Some(auth) = stored_auth else {
            return RegistryTestResponse {
                status: "unauthorized".into(),
                http_status: Some(status),
                message: "reachable for pulls, but push target requires credentials (none stored)".into(),
            };
        };
        let Some(realm) = auth_param(challenge, "realm") else {
            return RegistryTestResponse {
                status: "unauthorized".into(),
                http_status: Some(status),
                message: "push target returned Bearer challenge without realm".into(),
            };
        };
        let mut token_url = format!("{realm}?scope=repository:litebin-probe:push,pull");
        if let Some(service) = auth_param(challenge, "service") {
            token_url.push_str(&format!("&service={service}"));
        }
        let Ok(tok_resp) = client.get(&token_url).header("Authorization", format!("Basic {auth}")).send().await else {
            return RegistryTestResponse {
                status: "unreachable".into(),
                http_status: None,
                message: "could not reach registry token endpoint for push probe".into(),
            };
        };
        if !tok_resp.status().is_success() {
            return RegistryTestResponse {
                status: "unauthorized".into(),
                http_status: Some(tok_resp.status().as_u16()),
                message: "read works, but push token request was rejected".into(),
            };
        }
        let tok_json = tok_resp.json::<serde_json::Value>().await.unwrap_or_default();
        let Some(bearer) = tok_json.get("token").or_else(|| tok_json.get("access_token")).and_then(|v| v.as_str())
        else {
            return RegistryTestResponse {
                status: "unauthorized".into(),
                http_status: Some(status),
                message: "push token response did not contain a token".into(),
            };
        };
        let Ok(retry) = client.post(&upload_url).header("Authorization", format!("Bearer {bearer}")).send().await
        else {
            return RegistryTestResponse {
                status: "unreachable".into(),
                http_status: None,
                message: "push probe retry failed".into(),
            };
        };
        let retry_status = retry.status().as_u16();
        if retry_status == 202 || retry_status == 201 {
            if let Some(loc) = retry.headers().get("location").and_then(|v| v.to_str().ok()) {
                let del_url = if loc.starts_with("http") { loc.to_string() } else { format!("{scheme}://{host}{loc}") };
                let _ = client.delete(&del_url).header("Authorization", format!("Bearer {bearer}")).send().await;
            }
            return RegistryTestResponse {
                status: "ok".into(),
                http_status: Some(retry_status),
                message: "registry reachable, credentials accepted, push verified".into(),
            };
        }
        return RegistryTestResponse {
            status: if retry_status == 401 || retry_status == 403 { "unauthorized" } else { "unexpected_status" }
                .into(),
            http_status: Some(retry_status),
            message: format!("read works, but push probe returned HTTP {retry_status}"),
        };
    }

    if status == 202 || status == 201 {
        if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
            let del_url = if loc.starts_with("http") { loc.to_string() } else { format!("{scheme}://{host}{loc}") };
            let mut del = client.delete(&del_url);
            if let Some(auth) = stored_auth {
                del = del.header("Authorization", format!("Basic {auth}"));
            }
            let _ = del.send().await;
        }
        RegistryTestResponse {
            status: "ok".into(),
            http_status: Some(status),
            message: "registry reachable, credentials accepted, push verified".into(),
        }
    } else if status == 401 || status == 403 {
        RegistryTestResponse {
            status: "unauthorized".into(),
            http_status: Some(status),
            message: "read works, but push permission was rejected (401/403)".into(),
        }
    } else {
        RegistryTestResponse {
            status: "unexpected_status".into(),
            http_status: Some(status),
            message: format!("read works, but push probe returned HTTP {status}"),
        }
    }
}

/// Verify credentials at the token endpoint from a `/v2/` 401 Bearer challenge:
/// the scopeless token request `docker login` uses. 200 = accepted.
async fn token_auth_check(
    client: &reqwest::Client,
    challenge: &str,
    stored_auth: &str,
    ping_url: &str,
) -> RegistryTestResponse {
    let Some(realm) = auth_param(challenge, "realm") else {
        return RegistryTestResponse {
            status: "unauthorized".into(),
            http_status: Some(401),
            message: "registry requires token authentication but sent no token endpoint".into(),
        };
    };
    // No scope: ghcr.io rejects scopes for unknown repositories, and
    // credential verification doesn't need one.
    let mut token_url = realm;
    if let Some(service) = auth_param(challenge, "service") {
        token_url.push_str(&format!("?service={service}"));
    }
    let request = client.get(&token_url).header("Authorization", format!("Basic {stored_auth}"));
    match request.send().await {
        Ok(resp) if resp.status().as_u16() == 200 => RegistryTestResponse {
            status: "ok".into(),
            http_status: Some(200),
            message: "registry reachable, credentials verified at its token endpoint".into(),
        },
        Ok(resp) => RegistryTestResponse {
            status: "unauthorized".into(),
            http_status: Some(resp.status().as_u16()),
            message: "registry reachable, but the stored credentials were rejected".into(),
        },
        Err(e) => RegistryTestResponse {
            status: "unreachable".into(),
            http_status: None,
            message: format!("could not reach {ping_url}: {e}"),
        },
    }
}

/// Extract a `key="value"` pair from a WWW-Authenticate challenge
/// (`Bearer realm="https://auth.docker.io/token",service="registry.docker.io"`).
fn auth_param(challenge: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = challenge.find(&marker)? + marker.len();
    let rest = &challenge[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{auth_param, decode_auth_b64, resolve_auth_input, unset_other_push_defaults};

    #[test]
    fn parses_bearer_challenge_params() {
        let challenge = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#;
        assert_eq!(auth_param(challenge, "realm").as_deref(), Some("https://auth.docker.io/token"));
        assert_eq!(auth_param(challenge, "service").as_deref(), Some("registry.docker.io"));
        assert_eq!(auth_param(challenge, "scope"), None);
        assert_eq!(auth_param("Basic", "realm"), None);
    }

    #[test]
    fn username_password_is_encoded_to_base64() {
        let auth = resolve_auth_input(Some("user"), Some("pa:ss"), None).unwrap().unwrap();
        assert_eq!(auth, "dXNlcjpwYTpzcw=="); // base64 of "user:pa:ss"
        assert_eq!(litebin_common::registry::decode_auth(&auth), Some(("user".to_string(), "pa:ss".to_string())));
    }

    #[test]
    fn auth_input_requires_both_halves_and_passes_auth_b64_through() {
        assert!(resolve_auth_input(Some("user"), None, None).is_err());
        assert!(resolve_auth_input(None, Some("pass"), None).is_err());
        assert_eq!(resolve_auth_input(None, None, Some("dXNlcjpwYXNz")).unwrap().as_deref(), Some("dXNlcjpwYXNz"));
        assert_eq!(resolve_auth_input(None, None, None).unwrap(), None);
        // Empty strings mean "not provided", not "clear".
        assert_eq!(resolve_auth_input(Some("  "), None, Some("  ")).unwrap(), None);
    }

    #[test]
    fn decode_auth_b64_handles_valid_and_empty() {
        assert_eq!(decode_auth_b64(Some("dXNlcjpwYTpzcw==")), (Some("user".into()), Some("pa:ss".into())));
        assert_eq!(decode_auth_b64(Some("")), (None, None));
        assert_eq!(decode_auth_b64(None), (None, None));
    }

    #[tokio::test]
    async fn unset_other_push_defaults_keeps_only_selected() {
        let pool = crate::db::init_pool("sqlite::memory:").await.unwrap();
        let now = 1_700_000_000_i64;
        for (id, push_def) in [("r1", 1), ("r2", 1)] {
            sqlx::query(
                "INSERT INTO registries (id, name, url, is_default, is_push_default, is_builtin, created_at, updated_at) VALUES (?, ?, 'http://localhost:5010', 0, ?, 0, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(push_def)
            .bind(now)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        }
        unset_other_push_defaults(&pool, "r2").await.unwrap();
        let rows: Vec<(String, bool)> =
            sqlx::query_as("SELECT id, is_push_default FROM registries WHERE id IN ('r1', 'r2') ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("r1".into(), false), ("r2".into(), true)]);
    }
}
