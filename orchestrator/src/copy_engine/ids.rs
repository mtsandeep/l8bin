//! New project-id allocation for duplicate/import.
//!
//! The id is the subdomain (`{id}.{domain}`), so candidates must dodge
//! existing project ids, the reserved dashboard/poke subdomains, and alias
//! subdomains — the same checks `POST /projects` applies.

use sqlx::SqlitePool;

use crate::validation::is_valid_project_id;

pub const MAX_SUFFIX_ATTEMPTS: u32 = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdAllocation {
    pub id: String,
    /// True when the id was taken and a numeric suffix was applied.
    pub suffixed: bool,
}

/// Allocate a fresh project id. `requested` (or `base` when absent) wins when
/// free; otherwise candidates `{first}-2`, `{first}-3`, … are tried.
/// `reserved_subdomains` are platform words (dashboard, poke) that count as taken.
pub async fn allocate_project_id(
    db: &SqlitePool,
    reserved_subdomains: &[String],
    requested: Option<&str>,
    base: &str,
) -> anyhow::Result<IdAllocation> {
    let first = requested.unwrap_or(base);
    if !is_valid_project_id(first) {
        anyhow::bail!(
            "project id '{first}' must be 1-63 lowercase letters, digits, or hyphens (no leading/trailing hyphens)"
        );
    }
    if id_is_free(db, reserved_subdomains, first).await? {
        return Ok(IdAllocation { id: first.to_string(), suffixed: false });
    }
    for n in 2..=MAX_SUFFIX_ATTEMPTS {
        // Keep the total within the 63-char DNS-label budget.
        let suffix = format!("-{n}");
        let stem: String = first.chars().take(63 - suffix.len()).collect();
        let candidate = format!("{}{}", stem.trim_end_matches('-'), suffix);
        if is_valid_project_id(&candidate) && id_is_free(db, reserved_subdomains, &candidate).await? {
            return Ok(IdAllocation { id: candidate, suffixed: true });
        }
    }
    anyhow::bail!("no free project id for '{first}' after {MAX_SUFFIX_ATTEMPTS} attempts");
}

async fn id_is_free(db: &SqlitePool, reserved_subdomains: &[String], id: &str) -> anyhow::Result<bool> {
    if reserved_subdomains.iter().any(|r| r == id) {
        return Ok(false);
    }
    let project: Option<String> =
        sqlx::query_scalar("SELECT id FROM projects WHERE id = ?").bind(id).fetch_optional(db).await?;
    if project.is_some() {
        return Ok(false);
    }
    let alias: Option<String> =
        sqlx::query_scalar("SELECT subdomain FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(alias.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal in-memory DB with the two tables the allocator consults.
    async fn test_db() -> SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE project_routes (route_type TEXT, subdomain TEXT)").execute(&pool).await.unwrap();
        pool
    }

    async fn seed(pool: &SqlitePool, projects: &[&str], aliases: &[&str]) -> Vec<String> {
        for id in projects {
            sqlx::query("INSERT INTO projects (id) VALUES (?)").bind(id).execute(pool).await.unwrap();
        }
        for sub in aliases {
            sqlx::query("INSERT INTO project_routes (route_type, subdomain) VALUES ('alias', ?)")
                .bind(sub)
                .execute(pool)
                .await
                .unwrap();
        }
        vec!["dash".to_string(), "poke".to_string()]
    }

    #[tokio::test]
    async fn free_id_wins() {
        let pool = test_db().await;
        let reserved = seed(&pool, &[], &[]).await;
        let alloc = allocate_project_id(&pool, &reserved, Some("my-app"), "x-copy").await.unwrap();
        assert_eq!(alloc, IdAllocation { id: "my-app".into(), suffixed: false });
    }

    #[tokio::test]
    async fn taken_id_gets_numeric_suffix() {
        let pool = test_db().await;
        let reserved = seed(&pool, &["my-app"], &[]).await;
        let alloc = allocate_project_id(&pool, &reserved, Some("my-app"), "my-app").await.unwrap();
        assert_eq!(alloc, IdAllocation { id: "my-app-2".into(), suffixed: true });
    }

    #[tokio::test]
    async fn taken_suffix_rolls_forward() {
        let pool = test_db().await;
        let reserved = seed(&pool, &["my-app", "my-app-2"], &[]).await;
        let alloc = allocate_project_id(&pool, &reserved, Some("my-app"), "my-app").await.unwrap();
        assert_eq!(alloc, IdAllocation { id: "my-app-3".into(), suffixed: true });
    }

    #[tokio::test]
    async fn reserved_and_alias_subdomains_count_as_taken() {
        let pool = test_db().await;
        let reserved = seed(&pool, &["some-app"], &["alias-taken"]).await;
        let dash = allocate_project_id(&pool, &reserved, Some("dash"), "x").await.unwrap();
        assert_eq!(dash.id, "dash-2");
        let via_alias = allocate_project_id(&pool, &reserved, Some("alias-taken"), "x").await.unwrap();
        assert_eq!(via_alias.id, "alias-taken-2");
    }

    #[tokio::test]
    async fn long_ids_stay_within_dns_label_budget() {
        let pool = test_db().await;
        let long = "a".repeat(63);
        let reserved = seed(&pool, &[&long], &[]).await;
        let alloc = allocate_project_id(&pool, &reserved, Some(&long), "x").await.unwrap();
        assert_eq!(alloc.id.len(), 63);
        assert_eq!(alloc.id, format!("{}-2", "a".repeat(61)));
    }

    #[tokio::test]
    async fn invalid_requested_id_is_rejected() {
        let pool = test_db().await;
        let reserved = seed(&pool, &[], &[]).await;
        assert!(allocate_project_id(&pool, &reserved, Some("Bad_Id"), "x").await.is_err());
    }
}
