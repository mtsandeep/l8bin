use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// Re-export shared types from litebin-common
pub use litebin_common::types::{Node, Project};

/// User is defined locally because it needs AuthUser impl (orphan rule).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub is_admin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct DeployToken {
    pub id: String,
    pub user_id: String,
    pub project_id: Option<String>,
    #[allow(dead_code)]
    pub token_hash: String,
    pub name: Option<String>,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub scope: litebin_common::types::TokenScope,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DeployTokenResponse {
    pub id: String,
    pub name: Option<String>,
    pub project_id: Option<String>,
    pub scope: litebin_common::types::TokenScope,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
}

impl From<DeployToken> for DeployTokenResponse {
    fn from(t: DeployToken) -> Self {
        Self {
            id: t.id,
            name: t.name,
            project_id: t.project_id,
            scope: t.scope,
            last_used_at: t.last_used_at,
            expires_at: t.expires_at,
            created_at: t.created_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Registry {
    pub id: String,
    pub name: String,
    /// Canonical Docker auth serveraddress (`https://index.docker.io/v1/`).
    pub url: String,
    /// base64 `user:password`; NULL = anonymous pull.
    pub auth_b64: Option<String>,
    /// Ordered mirror endpoints as a JSON array; NULL/empty = none.
    pub mirrors: Option<String>,
    pub is_default: bool,
    pub is_builtin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Registry {
    /// Mirrors in preference order, each with its own optional credentials.
    pub fn mirror_list(&self) -> Vec<litebin_common::registry::MirrorConfig> {
        self.mirrors.as_deref().and_then(|mirrors| serde_json::from_str(mirrors).ok()).unwrap_or_default()
    }
}

/// API shape for a mirror — credentials are never returned in full.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MirrorResponse {
    pub url: String,
    /// Whether credentials are configured for this mirror.
    pub has_auth: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_hint: Option<String>,
}

impl From<&litebin_common::registry::MirrorConfig> for MirrorResponse {
    fn from(m: &litebin_common::registry::MirrorConfig) -> Self {
        let auth = m.auth.as_deref().filter(|auth| !auth.is_empty());
        Self {
            url: m.url.clone(),
            has_auth: auth.is_some(),
            auth_hint: auth
                .filter(|auth| auth.len() > 8)
                .and_then(|auth| auth.get(auth.len() - 4..))
                .map(str::to_string),
        }
    }
}

/// API shape for a registry — credentials are never returned in full.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RegistryResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Whether credentials are configured for this registry.
    pub has_auth: bool,
    /// Last characters of the stored credential, for visual confirmation only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_hint: Option<String>,
    /// Mirrors in preference order, before the registry itself.
    pub mirrors: Vec<MirrorResponse>,
    /// Whether image names without a registry host resolve through this registry.
    pub is_default: bool,
    pub is_builtin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Registry> for RegistryResponse {
    fn from(r: Registry) -> Self {
        let auth = r.auth_b64.as_deref().filter(|auth| !auth.is_empty());
        let mirrors = r.mirror_list().iter().map(MirrorResponse::from).collect();
        Self {
            id: r.id,
            name: r.name,
            url: r.url,
            has_auth: auth.is_some(),
            auth_hint: auth
                .filter(|auth| auth.len() > 8)
                .and_then(|auth| auth.get(auth.len() - 4..))
                .map(str::to_string),
            mirrors,
            is_default: r.is_default,
            is_builtin: r.is_builtin,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}
