use crate::AppState;
use crate::nodes;

#[derive(Debug)]
pub(super) enum TargetPreflightError {
    Selection(anyhow::Error),
    Eligibility(anyhow::Error),
}

pub(super) async fn resolve_target_before_mutation<F, Fut>(
    db: &sqlx::SqlitePool,
    sticky_node_id: Option<&str>,
    override_node_id: Option<String>,
    requires_host_network: bool,
    check_host_network: F,
) -> Result<String, TargetPreflightError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let target_node_id = nodes::selector::select_node_for_sticky(db, sticky_node_id, override_node_id)
        .await
        .map_err(TargetPreflightError::Selection)?;
    if requires_host_network {
        check_host_network(target_node_id.clone()).await.map_err(TargetPreflightError::Eligibility)?;
    }
    Ok(target_node_id)
}

pub(super) async fn require_live_host_network_target(state: &AppState, target_node_id: &str) -> anyhow::Result<()> {
    if target_node_id == "local" {
        let host = state.docker.host_info().await.ok();
        return litebin_common::docker::require_host_network_eligible(
            host.as_ref().and_then(|info| info.rootless),
            Some(3),
        );
    }

    let node = crate::routes::manage::get_node_from_db(&state.db, target_node_id)
        .await
        .map_err(|error| anyhow::anyhow!("failed to load selected node: {error:?}"))?;
    let client = nodes::client::get_node_client(&state.node_clients, target_node_id)
        .map_err(|error| anyhow::anyhow!("selected agent client is unavailable: {error:?}"))?;
    let agent = nodes::client::AgentClient::new(client, &node, &state.config);
    let health = agent
        .health()
        .await
        .map_err(|error| anyhow::anyhow!("failed to contact selected agent for host-network eligibility: {error}"))?;
    litebin_common::docker::require_host_network_eligible(health.docker_rootless, Some(health.protocol_version as i64))
}

#[cfg(test)]
mod tests {
    use super::{TargetPreflightError, resolve_target_before_mutation};
    use sqlx::SqlitePool;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    async fn preflight_db() -> SqlitePool {
        let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                architecture TEXT,
                version TEXT,
                public_ip TEXT,
                agent_port INTEGER NOT NULL DEFAULT 8443,
                region TEXT,
                status TEXT NOT NULL DEFAULT 'offline',
                total_memory INTEGER,
                total_cpu REAL,
                available_memory INTEGER,
                disk_free INTEGER,
                disk_total INTEGER,
                container_count INTEGER NOT NULL DEFAULT 0,
                last_seen_at INTEGER,
                fail_count INTEGER NOT NULL DEFAULT 0,
                agent_secret TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (
                id, name, host, status, created_at, updated_at
             ) VALUES ('local', 'Local', 'localhost', 'online', 1, 1)",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE mutation_guard (
                project_id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                node_id TEXT,
                artifact TEXT
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mutation_guard (project_id, status, node_id, artifact)
             VALUES ('existing', 'running', 'local', 'original')",
        )
        .execute(&db)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn host_network_eligibility_failure_precedes_deploy_mutations() {
        let db = preflight_db().await;
        let eligibility_checked = Arc::new(AtomicBool::new(false));
        let checked = eligibility_checked.clone();

        let result = resolve_target_before_mutation(&db, Some("local"), None, true, move |target_node_id| async move {
            checked.store(true, Ordering::SeqCst);
            assert_eq!(target_node_id, "local");
            anyhow::bail!("ineligible live host")
        })
        .await;

        assert!(matches!(result, Err(TargetPreflightError::Eligibility(_))));
        assert!(eligibility_checked.load(Ordering::SeqCst));
        let existing: (String, Option<String>, String) = sqlx::query_as(
            "SELECT status, node_id, artifact
             FROM mutation_guard WHERE project_id = 'existing'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(existing, ("running".into(), Some("local".into()), "original".into()));
        let new_project_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mutation_guard WHERE project_id = 'new-project'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(new_project_count, 0);
    }
}
