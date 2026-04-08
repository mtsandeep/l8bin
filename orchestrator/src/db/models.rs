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
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployTokenResponse {
    pub id: String,
    pub name: Option<String>,
    pub project_id: Option<String>,
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
            last_used_at: t.last_used_at,
            expires_at: t.expires_at,
            created_at: t.created_at,
        }
    }
}
