#[cfg(test)]
mod prop_tests {
    use proptest::prelude::*;
    use sqlx::SqlitePool;

    /// Set up an in-memory DB with migrations applied and a seed user.
    async fn setup_db() -> SqlitePool {
        let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("src/db/migrations").run(&db).await.unwrap();

        // Insert a seed user so the projects FK constraint is satisfied.
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, is_admin, created_at, updated_at)
             VALUES ('test-user', 'testuser', 'hash', 0, ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db)
        .await
        .unwrap();

        db
    }

    /// Insert a running project with the given settings.
    /// `last_active_at` is set far in the past so it is always idle.
    async fn insert_running_project(
        db: &SqlitePool,
        id: &str,
        auto_stop_enabled: bool,
        auto_stop_timeout_mins: i64,
        last_active_at: i64,
    ) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            r#"INSERT INTO projects
               (id, user_id, image, internal_port, status,
                auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled,
                last_active_at, created_at, updated_at)
               VALUES (?, 'test-user', 'test-image:latest', 8080, 'running',
                       ?, ?, 1,
                       ?, ?, ?)"#,
        )
        .bind(id)
        .bind(auto_stop_enabled as i64)
        .bind(auto_stop_timeout_mins)
        .bind(last_active_at)
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .unwrap();
    }

    /// Simulate the janitor's filtering logic directly against the DB:
    /// 1. Query running projects with auto_stop_enabled = 1
    /// 2. Apply the per-project idle check
    /// 3. Mark matching projects as stopped
    ///
    /// This mirrors the logic in `sweep()` without requiring Docker or Caddy.
    async fn run_sweep_logic(db: &SqlitePool) {
        let now = chrono::Utc::now().timestamp();

        let candidates = sqlx::query_as::<_, crate::db::models::Project>(
            "SELECT * FROM projects WHERE status = 'running' AND auto_stop_enabled = 1",
        )
        .fetch_all(db)
        .await
        .unwrap();

        for project in candidates {
            let timeout_secs = project.auto_stop_timeout_mins * 60;
            let is_idle = project
                .last_active_at
                .map(|t| now - t >= timeout_secs)
                .unwrap_or(true);

            if is_idle {
                sqlx::query(
                    "UPDATE projects SET status = 'stopped', updated_at = ? WHERE id = ?",
                )
                .bind(now)
                .bind(&project.id)
                .execute(db)
                .await
                .unwrap();
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Janitor stops the project iff `idle_secs_ago >= timeout_mins * 60`.
        #[test]
        fn prop_janitor_uses_per_project_timeout(
            timeout_mins in 1i64..=10080i64,
            idle_secs_ago in 0i64..=100000i64,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let db = setup_db().await;

                let now = chrono::Utc::now().timestamp();
                let last_active_at = now - idle_secs_ago;

                insert_running_project(
                    &db,
                    "proj-timeout",
                    true, // auto_stop_enabled
                    timeout_mins,
                    last_active_at,
                )
                .await;

                run_sweep_logic(&db).await;

                let row: (String,) =
                    sqlx::query_as("SELECT status FROM projects WHERE id = ?")
                        .bind("proj-timeout")
                        .fetch_one(&db)
                        .await
                        .unwrap();

                let status = &row.0;
                let should_stop = idle_secs_ago >= timeout_mins * 60;

                if should_stop {
                    prop_assert_eq!(
                        status.as_str(),
                        "stopped",
                        "project idle for {}s with timeout {}min ({} secs) should be stopped",
                        idle_secs_ago,
                        timeout_mins,
                        timeout_mins * 60
                    );
                } else {
                    prop_assert_eq!(
                        status.as_str(),
                        "running",
                        "project idle for {}s with timeout {}min ({} secs) should remain running",
                        idle_secs_ago,
                        timeout_mins,
                        timeout_mins * 60
                    );
                }

                Ok(())
            }).unwrap();
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10))]

        /// Janitor only stops auto-stop-enabled projects.
        #[test]
        fn prop_janitor_respects_auto_stop_enabled(
            // Generate 1–8 projects; each is (auto_stop_enabled, timeout_mins 1..=60)
            projects in prop::collection::vec(
                (any::<bool>(), 1i64..=60i64),
                1..=8,
            ),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let db = setup_db().await;

                // Use a last_active_at far in the past so every project is idle.
                let far_past = chrono::Utc::now().timestamp() - 86400; // 24 h ago

                for (idx, (auto_stop_enabled, timeout_mins)) in projects.iter().enumerate() {
                    let id = format!("proj-{}", idx);
                    insert_running_project(
                        &db,
                        &id,
                        *auto_stop_enabled,
                        *timeout_mins,
                        far_past,
                    )
                    .await;
                }

                // Run the sweep logic (no Docker/Caddy needed).
                run_sweep_logic(&db).await;

                // Verify post-sweep state.
                for (idx, (auto_stop_enabled, _)) in projects.iter().enumerate() {
                    let id = format!("proj-{}", idx);
                    let row: (String,) =
                        sqlx::query_as("SELECT status FROM projects WHERE id = ?")
                            .bind(&id)
                            .fetch_one(&db)
                            .await
                            .unwrap();

                    let status = &row.0;
                    if *auto_stop_enabled {
                        prop_assert_eq!(
                            status.as_str(),
                            "stopped",
                            "project {} with auto_stop_enabled=true should be stopped",
                            id
                        );
                    } else {
                        prop_assert_eq!(
                            status.as_str(),
                            "running",
                            "project {} with auto_stop_enabled=false should remain running",
                            id
                        );
                    }
                }

                Ok(())
            }).unwrap();
        }
    }
}
