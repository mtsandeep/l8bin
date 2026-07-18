//! Project capability grant helpers.

use axum::http::StatusCode;
use compose_bollard::{analyze_compose_yaml, FindingDisposition};
use litebin_common::capabilities::{
    capability_catalog, ProjectCapability, ProjectCapabilityGrant, ProjectCapabilityStatus,
};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;

/// List granted capability ids for a project.
pub async fn list_grants(
    db: &SqlitePool,
    project_id: &str,
) -> Result<Vec<ProjectCapabilityGrant>, sqlx::Error> {
    sqlx::query_as::<_, ProjectCapabilityGrant>(
        "SELECT project_id, capability, granted_at, granted_by \
         FROM project_capabilities WHERE project_id = ? ORDER BY capability",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
}

/// Return granted capability ids as a set of strings.
pub async fn granted_ids(
    db: &SqlitePool,
    project_id: &str,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT capability FROM project_capabilities WHERE project_id = ?",
    )
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
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_capabilities WHERE project_id = ? AND capability = ?",
    )
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
pub async fn revoke(
    db: &SqlitePool,
    project_id: &str,
    capability: ProjectCapability,
) -> Result<(), sqlx::Error> {
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
    let docker = has_capability(db, project_id, ProjectCapability::DockerObserve).await?;
    let raw = has_capability(db, project_id, ProjectCapability::RawPorts).await?;
    sqlx::query(
        "UPDATE projects SET allow_docker_access = ?, allow_raw_ports = ?, updated_at = ? WHERE id = ?",
    )
    .bind(docker)
    .bind(raw)
    .bind(chrono::Utc::now().timestamp())
    .bind(project_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Resolve effective flags for container runtime (prefer grants, fall back to legacy columns).
#[allow(dead_code)]
pub async fn effective_flags(
    db: &SqlitePool,
    project_id: &str,
    legacy_docker: bool,
    legacy_raw: bool,
) -> Result<(bool, bool), sqlx::Error> {
    let grants = granted_ids(db, project_id).await?;
    let docker = grants.contains(ProjectCapability::DockerObserve.id()) || legacy_docker;
    let raw = grants.contains(ProjectCapability::RawPorts.id()) || legacy_raw;
    Ok((docker, raw))
}

/// Capabilities required by a report that are not yet granted.
pub fn missing_capabilities(
    required: &[String],
    granted: &std::collections::HashSet<String>,
) -> Vec<String> {
    required
        .iter()
        .filter(|c| !granted.contains(c.as_str()))
        .cloned()
        .collect()
}

/// Build status list for the Capabilities settings tab.
pub async fn status_list(
    db: &SqlitePool,
    project_id: &str,
    requested_reasons: &HashMap<String, String>,
) -> Result<Vec<ProjectCapabilityStatus>, sqlx::Error> {
    let grants = list_grants(db, project_id).await?;
    let granted_map: HashMap<String, i64> = grants
        .into_iter()
        .map(|g| (g.capability, g.granted_at))
        .collect();

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

/// Analyze the project's stored compose.yaml for capability request reasons.
pub fn requested_reasons_from_compose(project_id: &str) -> HashMap<String, String> {
    let path = PathBuf::from("projects").join(project_id).join("compose.yaml");
    let Ok(yaml) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok((_, report)) = analyze_compose_yaml(&yaml, None, Some(project_id)) else {
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

/// Status list with compose-derived request reasons filled in.
pub async fn status_list_for_project(
    db: &SqlitePool,
    project_id: &str,
) -> Result<Vec<ProjectCapabilityStatus>, sqlx::Error> {
    let reasons = requested_reasons_from_compose(project_id);
    status_list(db, project_id, &reasons).await
}

/// Map a capability helper error into an HTTP response pair.
pub fn db_err(e: sqlx::Error) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("capability store error: {e}"),
    )
}
