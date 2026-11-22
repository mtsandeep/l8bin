//! Project capability grant helpers.

use axum::http::StatusCode;
use compose_bollard::{FindingDisposition, analyze_compose_yaml_for_workload};
use litebin_common::capabilities::{
    ProjectCapability, ProjectCapabilityGrant, ProjectCapabilityStatus, capability_catalog,
};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;

/// List granted capability ids for a project.
pub async fn list_grants(db: &SqlitePool, project_id: &str) -> Result<Vec<ProjectCapabilityGrant>, sqlx::Error> {
    sqlx::query_as::<_, ProjectCapabilityGrant>(
        "SELECT project_id, capability, granted_at, granted_by \
         FROM project_capabilities WHERE project_id = ? ORDER BY capability",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
}

/// Return granted capability ids as a set of strings.
pub async fn granted_ids(db: &SqlitePool, project_id: &str) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT capability FROM project_capabilities WHERE project_id = ?")
        .bind(project_id)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

/// True if the project has the given capability.
pub async fn has_capability(
    db: &SqlitePool,
    project_id: &str,
    capability: ProjectCapability,
) -> Result<bool, sqlx::Error> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_capabilities WHERE project_id = ? AND capability = ?")
            .bind(project_id)
            .bind(capability.id())
            .fetch_one(db)
            .await?;
    Ok(count > 0)
}

/// Grant one capability. Idempotent.
pub async fn grant(
    db: &SqlitePool,
    project_id: &str,
    capability: ProjectCapability,
    granted_by: Option<&str>,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO project_capabilities (project_id, capability, granted_at, granted_by) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(project_id, capability) DO NOTHING",
    )
    .bind(project_id)
    .bind(capability.id())
    .bind(now)
    .bind(granted_by)
    .execute(db)
    .await?;

    // Keep legacy boolean columns in sync during transition.
    sync_legacy_flags(db, project_id).await?;
    Ok(())
}

/// Grant multiple capabilities.
pub async fn grant_many(
    db: &SqlitePool,
    project_id: &str,
    capabilities: &[ProjectCapability],
    granted_by: Option<&str>,
) -> Result<(), sqlx::Error> {
    for c in capabilities {
        grant(db, project_id, *c, granted_by).await?;
    }
    Ok(())
}

/// Revoke one capability.
pub async fn revoke(db: &SqlitePool, project_id: &str, capability: ProjectCapability) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM project_capabilities WHERE project_id = ? AND capability = ?")
        .bind(project_id)
        .bind(capability.id())
        .execute(db)
        .await?;
    sync_legacy_flags(db, project_id).await?;
    Ok(())
}

/// Sync legacy `allow_*` boolean columns from capability grants.
async fn sync_legacy_flags(db: &SqlitePool, project_id: &str) -> Result<(), sqlx::Error> {
    let raw = has_capability(db, project_id, ProjectCapability::RawPorts).await?;
    sqlx::query("UPDATE projects SET allow_docker_access = ?, allow_raw_ports = ?, updated_at = ? WHERE id = ?")
        .bind(false)
        .bind(raw)
        .bind(chrono::Utc::now().timestamp())
        .bind(project_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Capabilities required by a report that are not yet granted.
pub fn missing_capabilities(required: &[String], granted: &std::collections::HashSet<String>) -> Vec<String> {
    required.iter().filter(|c| !granted.contains(c.as_str())).cloned().collect()
}

/// Build status list for the Capabilities settings tab.
pub async fn status_list(
    db: &SqlitePool,
    project_id: &str,
    requested_reasons: &HashMap<String, String>,
) -> Result<Vec<ProjectCapabilityStatus>, sqlx::Error> {
    let grants = list_grants(db, project_id).await?;
    let granted_map: HashMap<String, i64> = grants.into_iter().map(|g| (g.capability, g.granted_at)).collect();

    Ok(capability_catalog()
        .into_iter()
        .map(|info| {
            let granted_at = granted_map.get(&info.id).copied();
            ProjectCapabilityStatus {
                requested_reason: requested_reasons.get(&info.id).cloned(),
                granted: granted_at.is_some(),
                granted_at,
                info,
            }
        })
        .collect())
}

/// Analyze compose YAML text for capability request reasons.
fn requested_reasons_from_compose_yaml(yaml: &str, project_id: Option<&str>) -> HashMap<String, String> {
    let Ok((_, report)) = analyze_compose_yaml_for_workload(yaml, None, project_id, true) else {
        return HashMap::new();
    };

    let mut reasons: HashMap<String, String> = HashMap::new();
    for finding in report.findings {
        if finding.disposition != FindingDisposition::PermissionRequired {
            continue;
        }
        let Some(cap) = finding.capability else {
            continue;
        };
        reasons
            .entry(cap)
            .and_modify(|existing| {
                if !existing.contains(&finding.message) {
                    existing.push_str("; ");
                    existing.push_str(&finding.message);
                }
            })
            .or_insert(finding.message);
    }
    reasons
}

/// Analyze the project's stored compose.yaml for capability request reasons.
pub fn requested_reasons_from_compose(project_id: &str) -> HashMap<String, String> {
    let path = PathBuf::from("projects").join(project_id).join("compose.yaml");
    let Ok(yaml) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    requested_reasons_from_compose_yaml(&yaml, Some(project_id))
}

/// Status list with compose-derived request reasons filled in.
pub async fn status_list_for_project(
    db: &SqlitePool,
    project_id: &str,
) -> Result<Vec<ProjectCapabilityStatus>, sqlx::Error> {
    let reasons = requested_reasons_from_compose(project_id);
    status_list(db, project_id, &reasons).await
}

/// Capabilities the stored compose requires that are not granted — the start
/// gate. Empty for single-service projects (no stored compose) or when all
/// requirements are granted.
pub async fn missing_for_start(db: &SqlitePool, project_id: &str) -> Result<Vec<ProjectCapabilityStatus>, sqlx::Error> {
    Ok(status_list_for_project(db, project_id)
        .await?
        .into_iter()
        .filter(|s| !s.granted && s.requested_reason.is_some())
        .collect())
}

/// Map a capability helper error into an HTTP response pair.
pub fn db_err(e: sqlx::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("capability store error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_reasons_flag_required_capabilities_only() {
        let yaml = r#"
services:
  db:
    image: postgres
    ports:
      - "5432:5432"
  web:
    image: nginx
"#;
        let reasons = requested_reasons_from_compose_yaml(yaml, Some("proj"));
        assert!(reasons.contains_key("raw-ports"), "expected raw-ports request, got {reasons:?}");
        assert!(!reasons.contains_key("host-network"));
    }

    #[test]
    fn compose_reasons_empty_without_requests() {
        let yaml = "services:\n  web:\n    image: nginx\n";
        assert!(requested_reasons_from_compose_yaml(yaml, Some("proj")).is_empty());
    }

    #[tokio::test]
    async fn missing_for_start_lists_requested_but_ungranted() -> anyhow::Result<()> {
        let db = sqlx::SqlitePool::connect(":memory:").await?;
        sqlx::query(
            "CREATE TABLE project_capabilities (project_id TEXT, capability TEXT, granted_at INTEGER, granted_by TEXT, PRIMARY KEY (project_id, capability))",
        )
        .execute(&db)
        .await?;
        // `grant` syncs the legacy allow_* columns on the projects row.
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, updated_at INTEGER, allow_docker_access INTEGER, allow_raw_ports INTEGER)")
            .execute(&db)
            .await?;

        let yaml = "services:\n  db:\n    image: postgres\n    ports:\n      - \"5432:5432\"\n";
        let reasons = requested_reasons_from_compose_yaml(yaml, Some("proj"));

        // Nothing granted yet — the request is the blocker.
        let ungranted = status_list(&db, "proj", &reasons).await?;
        let missing: Vec<String> =
            ungranted.into_iter().filter(|s| !s.granted && s.requested_reason.is_some()).map(|s| s.info.id).collect();
        assert_eq!(missing, vec!["raw-ports".to_string()]);

        // Once granted, nothing is missing.
        grant(&db, "proj", litebin_common::capabilities::ProjectCapability::RawPorts, None).await?;
        let granted = status_list(&db, "proj", &reasons).await?;
        assert!(granted.into_iter().all(|s| s.granted || s.requested_reason.is_none()));

        Ok(())
    }
}
