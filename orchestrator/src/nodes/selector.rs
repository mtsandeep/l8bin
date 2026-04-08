use anyhow::anyhow;
use sqlx::SqlitePool;
use litebin_common::types::{Node, Project};

pub async fn select_node(
    db: &SqlitePool,
    project: &Project,
    override_node_id: Option<String>,
) -> anyhow::Result<String> {
    // 1. Override path — validate node exists and is online
    if let Some(override_id) = override_node_id {
        let node = sqlx::query_as::<_, Node>(
            "SELECT * FROM nodes WHERE id = ? AND status = 'online'"
        )
        .bind(&override_id)
        .fetch_optional(db)
        .await?;

        match node {
            Some(_) => return Ok(override_id),
            None => return Err(anyhow!("override node '{}' is not available", override_id)),
        }
    }

    // 2. Sticky path: if project has a node_id referencing an online node
    if let Some(ref node_id) = project.node_id {
        let node = sqlx::query_as::<_, Node>(
            "SELECT * FROM nodes WHERE id = ? AND status = 'online'"
        )
        .bind(node_id)
        .fetch_optional(db)
        .await?;

        if node.is_some() {
            return Ok(node_id.clone());
        }
    }

    // 3. Least-loaded path: score nodes by memory usage + container count
    let nodes = sqlx::query_as::<_, Node>(
        "SELECT * FROM nodes WHERE status = 'online'"
    )
    .fetch_all(db)
    .await?;

    if nodes.is_empty() {
        return Err(anyhow!("no available nodes"));
    }

    const MIN_DISK_FREE: i64 = 2 * 1024 * 1024 * 1024; // 2 GB

    let candidates: Vec<_> = nodes
        .into_iter()
        .filter(|n| n.disk_free.unwrap_or(0) >= MIN_DISK_FREE)
        .collect();

    if candidates.is_empty() {
        return Err(anyhow!("no available nodes (all nodes filtered: insufficient disk space)"));
    }

    let best = candidates
        .into_iter()
        .min_by_key(|n| {
            let total = n.total_memory.unwrap_or(0).max(1);
            let available = n.available_memory.unwrap_or(0);
            let mem_used_pct = ((total.saturating_sub(available)) * 100) / total;
            mem_used_pct + (n.container_count * 10)
        });

    match best {
        Some(node) => Ok(node.id),
        None => Err(anyhow!("no available nodes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    async fn test_db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
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
            )"
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn make_project(node_id: Option<&str>) -> Project {
        Project {
            id: "proj1".to_string(),
            user_id: "user1".to_string(),
            name: None,
            description: None,
            image: Some("img".to_string()),
            internal_port: Some(3000),
            mapped_port: None,
            container_id: None,
            node_id: node_id.map(|s| s.to_string()),
            status: "stopped".to_string(),
            last_active_at: None,
            auto_stop_enabled: true,
            auto_stop_timeout_mins: 15,
            auto_start_enabled: true,
            cmd: None,
            memory_limit_mb: None,
            cpu_limit: None,
            custom_domain: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    proptest! {
        #[test]
        fn prop_scheduler_sticky(node_id in "[a-z]{4,8}") {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, total_memory, created_at, updated_at)
                     VALUES (?, 'N', 'localhost', 8443, 'online', 0, 1000000, ?, ?)"
                )
                .bind(&node_id)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                let project = make_project(Some(&node_id));
                let result = select_node(&pool, &project, None).await.unwrap();

                prop_assert_eq!(result, node_id);
                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #[test]
        fn prop_scheduler_least_loaded(
            total_a in 4000i64..=8000i64,
            used_pct_a in 10i64..=40i64,
            total_b in 4000i64..=8000i64,
            used_pct_b in 50i64..=90i64,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();
                let disk = 10i64 * 1024 * 1024 * 1024; // 10 GB — above threshold

                let avail_a = total_a * (100 - used_pct_a) / 100;
                let avail_b = total_b * (100 - used_pct_b) / 100;

                // node-a has lower usage, node-b has higher usage
                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, total_memory, available_memory, disk_free, container_count, created_at, updated_at)
                     VALUES ('node-a', 'A', 'localhost', 8443, 'online', 0, ?, ?, ?, 0, ?, ?)"
                )
                .bind(total_a)
                .bind(avail_a)
                .bind(disk)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, total_memory, available_memory, disk_free, container_count, created_at, updated_at)
                     VALUES ('node-b', 'B', 'localhost', 8444, 'online', 0, ?, ?, ?, 0, ?, ?)"
                )
                .bind(total_b)
                .bind(avail_b)
                .bind(disk)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Should pick node-a (lower usage)
                let project = make_project(None);
                let result = select_node(&pool, &project, None).await.unwrap();

                prop_assert_eq!(result.as_str(), "node-a");
                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #[test]
        fn prop_scheduler_override(
            sticky_node in "[a-z]{4,6}",
            override_node in "[a-z]{7,10}",
        ) {
            // Ensure they're different
            if sticky_node == override_node {
                return Ok(());
            }

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = test_db().await;
                let now = chrono::Utc::now().timestamp();

                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, total_memory, created_at, updated_at)
                     VALUES (?, 'N', 'localhost', 8443, 'online', 0, 1000000, ?, ?)"
                )
                .bind(&sticky_node)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                sqlx::query(
                    "INSERT INTO nodes (id, name, host, agent_port, status, fail_count, total_memory, created_at, updated_at)
                     VALUES (?, 'N2', 'localhost', 8444, 'online', 0, 500000, ?, ?)"
                )
                .bind(&override_node)
                .bind(now)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();

                // Project with sticky node_id, but override provided
                let project = make_project(Some(&sticky_node));
                let result = select_node(&pool, &project, Some(override_node.clone())).await.unwrap();

                prop_assert_eq!(result, override_node);
                Ok(())
            }).unwrap();
        }
    }
}
