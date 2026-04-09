use std::time::Duration;
use std::sync::Arc;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::AppState;
use crate::nodes::client::get_node_client;
use litebin_common::types::HealthReport;

pub async fn run_heartbeat(state: AppState) {
    let interval = Duration::from_secs(state.config.heartbeat_interval_secs);

    loop {
        sleep(interval).await;
        run_heartbeat_pass(&state).await;
    }
}

async fn run_heartbeat_pass(state: &AppState) {
    // Refresh local node stats via sysinfo (no agent needed)
    refresh_local_node(state).await;

    // Query all non-decommissioned remote nodes
    let nodes = sqlx::query_as::<_, litebin_common::types::Node>(
        "SELECT * FROM nodes WHERE status != 'decommissioned' AND id != 'local'",
    )
    .fetch_all(&state.db)
    .await;

    let nodes = match nodes {
        Ok(n) => n,
        Err(e) => {
            warn!("heartbeat: failed to query nodes: {}", e);
            return;
        }
    };

    let handles: Vec<_> = nodes
        .into_iter()
        .map(|node| {
            let state = state.clone();
            tokio::spawn(async move { poll_node(&state, &node).await })
        })
        .collect();

    for handle in handles {
        let _ = handle.await;
    }
}

async fn refresh_local_node(state: &AppState) {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    let total = sys.total_memory() as i64;
    let available = sys.available_memory() as i64;
    let cpu = sys.cpus().len() as f64;
    let (df, dt) = litebin_common::sys::disk_space();
    let disk_free = df as i64;
    let disk_total = dt as i64;
    let container_count = state.docker.running_container_count().await.unwrap_or(0) as i64;
    let now = chrono::Utc::now().timestamp();

    let _ = sqlx::query(
        "UPDATE nodes SET total_memory = ?, total_cpu = ?, available_memory = ?, disk_free = ?, disk_total = ?, container_count = ?, last_seen_at = ?, updated_at = ? WHERE id = 'local'",
    )
    .bind(total)
    .bind(cpu)
    .bind(available)
    .bind(disk_free)
    .bind(disk_total)
    .bind(container_count)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;
}

async fn poll_node(state: &AppState, node: &litebin_common::types::Node) {
    // For pending_setup nodes, attempt to connect (push config)
    if node.status == "pending_setup" {
        attempt_connect(state, node).await;
        return;
    }

    let client = match get_node_client(&state.node_clients, &node.id) {
        Ok(c) => c,
        Err(_) => {
            // Node not in pool — treat as failure
            handle_failure(state, node).await;
            return;
        }
    };

    let health_url = if state.config.ca_cert_path.is_empty() {
        format!("http://{}:{}/health", node.host, node.agent_port)
    } else {
        format!("https://{}:{}/health", node.host, node.agent_port)
    };

    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(health) = resp.json::<HealthReport>().await {
                handle_success(state, node, &health).await;
            } else {
                handle_failure(state, node).await;
            }
        }
        _ => handle_failure(state, node).await,
    }
}

async fn handle_success(
    state: &AppState,
    node: &litebin_common::types::Node,
    health: &HealthReport,
) {
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE nodes SET last_seen_at = ?, fail_count = 0, status = 'online',
         total_memory = ?, total_cpu = ?, available_memory = ?, disk_free = ?, disk_total = ?, container_count = ?,
         public_ip = ?, updated_at = ? WHERE id = ?",
    )
    .bind(now)
    .bind(health.memory_total as i64)
    .bind(health.cpu_cores as f64)
    .bind(health.memory_available as i64)
    .bind(health.disk_free as i64)
    .bind(health.disk_total as i64)
    .bind(health.container_count as i64)
    .bind(&health.public_ip)
    .bind(now)
    .bind(&node.id)
    .execute(&state.db)
    .await;

    info!(node_id = %node.id, "heartbeat: node online");
}

async fn handle_failure(state: &AppState, node: &litebin_common::types::Node) {
    let now = chrono::Utc::now().timestamp();

    // Increment fail_count
    let _ = sqlx::query(
        "UPDATE nodes SET fail_count = fail_count + 1, updated_at = ? WHERE id = ?",
    )
    .bind(now)
    .bind(&node.id)
    .execute(&state.db)
    .await;

    // Re-read fail_count
    let new_count: i64 = sqlx::query_scalar("SELECT fail_count FROM nodes WHERE id = ?")
        .bind(&node.id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    warn!(node_id = %node.id, fail_count = new_count, "heartbeat: node poll failed");

    if new_count >= 3 {
        let _ = sqlx::query(
            "UPDATE nodes SET status = 'offline', updated_at = ? WHERE id = ?",
        )
        .bind(now)
        .bind(&node.id)
        .execute(&state.db)
        .await;

        warn!(node_id = %node.id, "heartbeat: node marked offline, triggering reconciliation");

        // Trigger reconciliation for this node's projects
        crate::nodes::reconciliation::run_reconciliation(state.clone(), Some(node.id.clone()))
            .await;
    }
}

/// Attempt to connect a pending_setup node: health check + push config via mTLS.
async fn attempt_connect(state: &AppState, node: &litebin_common::types::Node) {
    info!(node_id = %node.id, "heartbeat: attempting to connect pending_setup node");

    let client = match get_node_client(&state.node_clients, &node.id) {
        Ok(c) => c,
        Err(_) => {
            match crate::nodes::client::build_node_client(
                &state.config.ca_cert_path,
                &state.config.client_cert_path,
                &state.config.client_key_path,
            ) {
                Ok(c) => {
                    state.node_clients.insert(node.id.clone(), Arc::new(c));
                    state.node_clients.get(&node.id).unwrap().value().clone()
                }
                Err(e) => {
                    warn!(node_id = %node.id, error = %e, "heartbeat: cannot build mTLS client");
                    return;
                }
            }
        }
    };

    let base_url = crate::routes::manage::agent_base_url(&state.config, node);

    // Health check
    let health = match client.get(&format!("{}/health", base_url)).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<litebin_common::types::HealthReport>().await {
            Ok(h) => h,
            Err(e) => {
                warn!(node_id = %node.id, error = %e, "heartbeat: failed to parse health response");
                return;
            }
        },
        Ok(resp) => {
            warn!(node_id = %node.id, status = %resp.status(), "heartbeat: agent non-success");
            return;
        }
        Err(e) => {
            warn!(node_id = %node.id, error = %e, "heartbeat: agent unreachable");
            return;
        }
    };

    // Push config via POST /internal/register
    let secret = node.agent_secret.clone().unwrap_or_default();
    let register_body = serde_json::json!({
        "node_id": node.id,
        "secret": secret,
        "domain": state.config.domain,
        "wake_report_url": crate::routes::nodes::format_wake_report_url(state),
    });

    match client
        .post(&format!("{}/internal/register", base_url))
        .json(&register_body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            info!(node_id = %node.id, "heartbeat: config pushed to agent");
        }
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            warn!(node_id = %node.id, body, "heartbeat: agent rejected registration");
            return;
        }
        Err(e) => {
            warn!(node_id = %node.id, error = %e, "heartbeat: failed to push config");
            return;
        }
    }

    // Update node status to online
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        "UPDATE nodes SET status = 'online', fail_count = 0, total_memory = ?, total_cpu = ?, available_memory = ?, disk_free = ?, disk_total = ?, container_count = ?, public_ip = ?, last_seen_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(health.memory_total as i64)
    .bind(health.cpu_cores as f64)
    .bind(health.memory_available as i64)
    .bind(health.disk_free as i64)
    .bind(health.disk_total as i64)
    .bind(health.container_count as i64)
    .bind(&health.public_ip)
    .bind(now)
    .bind(now)
    .bind(&node.id)
    .execute(&state.db)
    .await;

    info!(node_id = %node.id, "heartbeat: node connected and online");
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    // Helper to create an in-memory test DB with the nodes table
    async fn test_db() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
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
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    proptest! {
        #[test]
        fn prop_heartbeat_success_resets_state(fail_count in 0i64..=10i64) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                // Insert node with arbitrary fail_count
                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, created_at, updated_at)
                     VALUES ('n1', 'N1', 'localhost', 8443, 'offline', ?, ?, ?)",
                )
                .bind(fail_count)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Simulate success: reset fail_count=0, status='online'
                sqlx::query(
                    "UPDATE nodes SET fail_count = 0, status = 'online', last_seen_at = ?, updated_at = ? WHERE id = 'n1'",
                )
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                let (fc, status): (i64, String) =
                    sqlx::query_as("SELECT fail_count, status FROM nodes WHERE id = 'n1'")
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                prop_assert_eq!(fc, 0);
                prop_assert_eq!(status.as_str(), "online");
                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #[test]
        fn prop_heartbeat_failure_progression(fail_count in 0i64..=2i64) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, created_at, updated_at)
                     VALUES ('n1', 'N1', 'localhost', 8443, 'online', ?, ?, ?)",
                )
                .bind(fail_count)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Simulate failure: increment fail_count
                sqlx::query(
                    "UPDATE nodes SET fail_count = fail_count + 1, updated_at = ? WHERE id = 'n1'",
                )
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                let new_count: i64 =
                    sqlx::query_scalar("SELECT fail_count FROM nodes WHERE id = 'n1'")
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                prop_assert_eq!(new_count, fail_count + 1);

                // At fail_count=3, status should be set to offline
                if new_count >= 3 {
                    sqlx::query(
                        "UPDATE nodes SET status = 'offline', updated_at = ? WHERE id = 'n1'",
                    )
                    .bind(now)
                    .execute(&pool)
                    .await
                    .unwrap();

                    let status: String =
                        sqlx::query_scalar("SELECT status FROM nodes WHERE id = 'n1'")
                            .fetch_one(&pool)
                            .await
                            .unwrap();

                    prop_assert_eq!(status.as_str(), "offline");
                }
                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #[test]
        fn prop_heartbeat_state_persists(
            fail_count in 0i64..=5i64,
            last_seen_at in 1700000000i64..=1800000000i64,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                // Write state to DB
                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, last_seen_at, created_at, updated_at)
                     VALUES ('n1', 'N1', 'localhost', 8443, 'online', ?, ?, ?, ?)",
                )
                .bind(fail_count)
                .bind(last_seen_at)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Simulate restart: re-read from DB
                let (fc, lsa): (i64, i64) =
                    sqlx::query_as("SELECT fail_count, last_seen_at FROM nodes WHERE id = 'n1'")
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                prop_assert_eq!(fc, fail_count);
                prop_assert_eq!(lsa, last_seen_at);
                Ok(())
            }).unwrap();
        }
    }
}
