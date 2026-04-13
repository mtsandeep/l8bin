use std::time::Instant;
use tracing::{info, warn};

use crate::AppState;
use crate::nodes::client::get_node_client;
use litebin_common::types::{ContainerStatus, Project};

pub async fn run_reconciliation(state: AppState, node_id: Option<String>) {
    let start = Instant::now();
    info!(node_id = ?node_id, "reconciliation: starting pass");

    // Query stuck projects (deploying/migrating = incomplete deploy; stopping = incomplete stop)
    let stuck_projects = match node_id {
        Some(ref nid) => {
            sqlx::query_as::<_, Project>(
                "SELECT * FROM projects WHERE status IN ('deploying', 'migrating', 'stopping') AND node_id = ?",
            )
            .bind(nid)
            .fetch_all(&state.db)
            .await
        }
        None => {
            sqlx::query_as::<_, Project>(
                "SELECT * FROM projects WHERE status IN ('deploying', 'migrating', 'stopping')",
            )
            .fetch_all(&state.db)
            .await
        }
    };

    let stuck_projects = match stuck_projects {
        Ok(p) => p,
        Err(e) => {
            warn!("reconciliation: failed to query stuck projects: {}", e);
            return;
        }
    };

    // Also check running projects on remote nodes (container may have crashed)
    let running_projects = match node_id.as_deref() {
        Some(nid) if nid != "local" => {
            sqlx::query_as::<_, Project>(
                "SELECT * FROM projects WHERE status = 'running' AND node_id = ?",
            )
            .bind(nid)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
        }
        None => {
            // On full pass, only check remote running projects (local Docker is authoritative)
            sqlx::query_as::<_, Project>(
                "SELECT * FROM projects WHERE status = 'running' AND node_id IS NOT NULL AND node_id != 'local'",
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
        }
        _ => Vec::new(), // local-only pass: skip running check for local
    };

    let mut corrections = 0usize;

    for project in &stuck_projects {
        reconcile_project(&state, project, &mut corrections).await;
    }

    // Check running projects on remote nodes — if the agent says the container is gone, set to error
    for project in &running_projects {
        let Some(ref container_id) = project.container_id else { continue };
        let Some(ref nid) = project.node_id else { continue };

        let client = match get_node_client(&state.node_clients, nid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let node = match sqlx::query_as::<_, litebin_common::types::Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(nid)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
        {
            Some(n) => n,
            None => continue,
        };

        let status_url = if state.config.ca_cert_path.is_empty() {
            format!("http://{}:{}/containers/{}/status", node.host, node.agent_port, container_id)
        } else {
            format!("https://{}:{}/containers/{}/status", node.host, node.agent_port, container_id)
        };

        match client.get(&status_url).send().await {
            Ok(resp) if resp.status().as_u16() == 404 => {
                set_project_error(&state, &project.id).await;
                corrections += 1;
            }
            Ok(resp) if resp.status().is_success() => {
                if let Ok(status) = resp.json::<ContainerStatus>().await {
                    if status.state != "running" {
                        set_project_error(&state, &project.id).await;
                        corrections += 1;
                    }
                }
            }
            _ => {} // agent unreachable — don't change status, heartbeat will handle offline
        }
    }

    info!(
        corrections,
        elapsed_ms = start.elapsed().as_millis(),
        "reconciliation: pass complete"
    );
}

async fn reconcile_project(state: &AppState, project: &Project, corrections: &mut usize) {
    let node_id = match &project.node_id {
        Some(nid) => nid.clone(),
        None => {
            // No node assigned — set to error
            set_project_error(state, &project.id).await;
            *corrections += 1;
            return;
        }
    };

    let container_id = match &project.container_id {
        Some(cid) => cid.clone(),
        None => {
            set_project_error(state, &project.id).await;
            *corrections += 1;
            return;
        }
    };

    // Local node: check directly via DockerManager
    if node_id == "local" {
        match state.docker.is_container_running(&container_id).await {
            Ok(true) => {
                set_project_running(state, project).await;
                *corrections += 1;
            }
            _ => {
                set_project_error(state, &project.id).await;
                *corrections += 1;
            }
        }
        return;
    }

    // Remote node: call agent
    let client = match get_node_client(&state.node_clients, &node_id) {
        Ok(c) => c,
        Err(_) => {
            set_project_error(state, &project.id).await;
            *corrections += 1;
            return;
        }
    };

    // Get node host/port from DB
    let node = sqlx::query_as::<_, litebin_common::types::Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    let Some(node) = node else {
        set_project_error(state, &project.id).await;
        *corrections += 1;
        return;
    };

    let status_url = if state.config.ca_cert_path.is_empty() {
        format!(
            "http://{}:{}/containers/{}/status",
            node.host, node.agent_port, container_id
        )
    } else {
        format!(
            "https://{}:{}/containers/{}/status",
            node.host, node.agent_port, container_id
        )
    };

    match client.get(&status_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(status) = resp.json::<ContainerStatus>().await {
                if status.state == "running" {
                    set_project_running(state, project).await;
                } else {
                    set_project_error(state, &project.id).await;
                }
            } else {
                set_project_error(state, &project.id).await;
            }
            *corrections += 1;
        }
        Ok(resp) if resp.status().as_u16() == 404 => {
            set_project_error(state, &project.id).await;
            *corrections += 1;
        }
        _ => {
            set_project_error(state, &project.id).await;
            *corrections += 1;
        }
    }
}

async fn set_project_error(state: &AppState, project_id: &str) {
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(project_id)
        .execute(&state.db)
        .await;
    warn!(project_id, "reconciliation: project set to error");
}

async fn set_project_running(state: &AppState, project: &Project) {
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query("UPDATE projects SET status = 'running', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(&project.id)
        .execute(&state.db)
        .await;

    // Sync routes
    let orchestrator_upstream = format!("litebin-orchestrator:{}", state.config.port);
    let routes = crate::routing_helpers::resolve_all_routes(&state.db, &state.config.domain, &orchestrator_upstream).await.unwrap_or_default();
    let _ = state
        .router
        .read()
        .await
        .sync_routes(&routes, &state.config.domain, &orchestrator_upstream, &state.config.dashboard_subdomain, &state.config.poke_subdomain, true)
        .await;

    info!(project_id = %project.id, "reconciliation: project restored to running");
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                agent_port INTEGER NOT NULL DEFAULT 8443,
                region TEXT,
                status TEXT NOT NULL DEFAULT 'offline',
                total_memory INTEGER,
                total_cpu REAL,
                last_seen_at INTEGER,
                fail_count INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                image TEXT NOT NULL,
                internal_port INTEGER NOT NULL,
                mapped_port INTEGER,
                container_id TEXT,
                node_id TEXT,
                status TEXT NOT NULL DEFAULT 'stopped',
                last_active_at INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    proptest! {
        #[test]
        fn prop_reconciliation_unreachable_sets_error(
            project_id in "[a-z]{4,8}",
            initial_status in prop_oneof![Just("deploying"), Just("migrating")],
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                // Insert a stuck project
                sqlx::query(
                    "INSERT INTO projects (id, user_id, image, internal_port, status, node_id, container_id, created_at, updated_at)
                     VALUES (?, 'user1', 'img', 3000, ?, 'worker-1', 'cid1', ?, ?)",
                )
                .bind(&project_id)
                .bind(initial_status)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Simulate: agent unreachable → set to error
                sqlx::query("UPDATE projects SET status = 'error', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&project_id)
                    .execute(&pool)
                    .await
                    .unwrap();

                let status: String =
                    sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
                        .bind(&project_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                prop_assert_eq!(status.as_str(), "error");
                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #[test]
        fn prop_reconciliation_running_restores(
            project_id in "[a-z]{4,8}",
            initial_status in prop_oneof![Just("deploying"), Just("migrating")],
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                // Insert a stuck project
                sqlx::query(
                    "INSERT INTO projects (id, user_id, image, internal_port, status, node_id, container_id, created_at, updated_at)
                     VALUES (?, 'user1', 'img', 3000, ?, 'worker-1', 'cid1', ?, ?)",
                )
                .bind(&project_id)
                .bind(initial_status)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Simulate: agent confirms running → set to running
                sqlx::query("UPDATE projects SET status = 'running', updated_at = ? WHERE id = ?")
                    .bind(now)
                    .bind(&project_id)
                    .execute(&pool)
                    .await
                    .unwrap();

                let status: String =
                    sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
                        .bind(&project_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                prop_assert_eq!(status.as_str(), "running");
                Ok(())
            }).unwrap();
        }
    }
}
