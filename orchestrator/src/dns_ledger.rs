//! Ledger of Cloudflare DNS records LiteBin created (`dns_records` table).
//!
//! Deletion decisions are made exclusively from this table — never by
//! suffix-sweeping the zone — so records the user created themselves (e.g.
//! `mail.{domain}` at a zone apex) are never touched by syncs, domain
//! changes, or Cloudflare cleanup.

use std::collections::HashSet;

use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub name: String,
    pub record_id: String,
}

pub async fn rows(db: &SqlitePool) -> anyhow::Result<Vec<LedgerRow>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, record_id FROM dns_records").fetch_all(db).await?;
    Ok(rows.into_iter().map(|(name, record_id)| LedgerRow { name, record_id }).collect())
}

/// Remember a record we created or updated. An empty `record_id` is skipped
/// (the caller could not determine one — nothing to delete by later).
pub async fn upsert(db: &SqlitePool, name: &str, record_id: &str) -> anyhow::Result<()> {
    if record_id.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO dns_records (name, record_id, updated_at) VALUES (?, ?, datetime('now'))
         ON CONFLICT(name) DO UPDATE SET record_id = excluded.record_id, updated_at = datetime('now')",
    )
    .bind(name)
    .bind(record_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn remove(db: &SqlitePool, name: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM dns_records WHERE name = ?").bind(name).execute(db).await?;
    Ok(())
}

/// Ledger rows whose name is no longer desired — the only records deletion
/// may touch. Pure so the diff is unit-testable without a DB.
pub fn stale<'a>(rows: &'a [LedgerRow], desired_names: &HashSet<&str>) -> Vec<&'a LedgerRow> {
    rows.iter().filter(|r| !desired_names.contains(r.name.as_str())).collect()
}

/// Rows recorded under `.{domain}` (or equal to it) — used by the
/// domain-change job to drop the old domain's managed records.
pub fn under_domain<'a>(rows: &'a [LedgerRow], domain: &str) -> Vec<&'a LedgerRow> {
    let suffix = format!(".{domain}");
    rows.iter().filter(|r| r.name == domain || r.name.ends_with(&suffix)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, id: &str) -> LedgerRow {
        LedgerRow { name: name.to_string(), record_id: id.to_string() }
    }

    #[test]
    fn stale_excludes_desired_names() {
        let rows = vec![row("app.example.com", "1"), row("gone.example.com", "2"), row("mail.example.com", "3")];
        let desired: HashSet<&str> = ["app.example.com", "mail.example.com"].into_iter().collect();
        let stale = stale(&rows, &desired);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "gone.example.com");
    }

    #[test]
    fn under_domain_matches_suffix_not_substrings() {
        let rows = vec![
            row("app.example.com", "1"),
            row("example.com", "2"),
            row("notexample.com", "3"), // suffix must not match partial labels
            row("app.other.com", "4"),  // different domain entirely
        ];
        let matched = under_domain(&rows, "example.com");
        let names: Vec<&str> = matched.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["app.example.com", "example.com"]);
    }
}
