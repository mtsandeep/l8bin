//! GitHub connect-and-deploy routes: connection management, repo browsing,
//! per-project links, and deploy status.

pub mod connect;
pub mod project_link;
pub mod repos;
pub mod runs;
pub mod webhook;

use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Split "owner/repo" into its parts, rejecting malformed input (path traversal).
pub(crate) fn split_repo(full_name: &str) -> Result<(String, String), (StatusCode, String)> {
    let bad = || (StatusCode::BAD_REQUEST, format!("expected repository as 'owner/repo', got '{full_name}'"));
    let (owner, repo) = full_name.trim().split_once('/').ok_or_else(bad)?;
    let valid_owner = !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let valid_repo = !repo.is_empty() && repo.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if valid_owner && valid_repo { Ok((owner.to_string(), repo.to_string())) } else { Err(bad()) }
}

/// Mint a project-scoped deploy token for a GitHub link. Mirrors
/// routes::deploy_tokens::create_token's minting, minus its HTTP surface —
/// plaintext exists only in the caller's memory (it goes straight into the
/// repo secret we PUT next).
pub(crate) async fn mint_project_token(
    db: &SqlitePool,
    user_id: &str,
    project_id: &str,
    name: &str,
) -> Result<(String, String), sqlx::Error> {
    let token_bytes: [u8; 32] = rand::random();
    let token = hex::encode(token_bytes);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let token_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO deploy_tokens (id, user_id, project_id, token_hash, name, created_at, expires_at, scope) VALUES (?, ?, ?, ?, ?, ?, NULL, 'deploy')",
    )
    .bind(&token_id)
    .bind(user_id)
    .bind(project_id)
    .bind(&token_hash)
    .bind(name)
    .bind(now)
    .execute(db)
    .await?;
    Ok((token_id, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_valid_repo_names() {
        assert_eq!(split_repo("user/repo").unwrap(), ("user".into(), "repo".into()));
        assert_eq!(split_repo("my-org/my.repo_v2").unwrap(), ("my-org".into(), "my.repo_v2".into()));
    }

    #[test]
    fn rejects_malformed_repo_names() {
        assert!(split_repo("repo").is_err());
        assert!(split_repo("a/b/c").is_err());
        assert!(split_repo("../etc").is_err());
        assert!(split_repo("").is_err());
        assert!(split_repo("user/").is_err());
    }
}
