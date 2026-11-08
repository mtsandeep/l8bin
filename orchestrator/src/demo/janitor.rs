//! Demo janitor — the demo lifecycle loop (a sibling of the scale-to-zero
//! janitor). Every tick: scheduled shared-mode resets, session hard-TTL
//! expiry (teardown is a hard kill — no drain grace), prewarm refills, and
//! self-healing of the edit/shared instances.

use crate::demo;
use crate::AppState;

const TICK_SECS: u64 = 60;

/// Spawn the demo janitor loop (called once from main).
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state).await {
                tracing::warn!(error = %e, "demo janitor tick failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
        }
    });
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    scheduled_resets(state).await?;
    expire_sessions(state).await;
    prewarm_refill(state).await;
    ensure_long_lived(state).await;
    sweep_orphans(state).await;
    Ok(())
}

/// Demo children whose parent has no demo_config — leftovers of a disable
/// that raced a janitor tick (or manual deletion). Reaped so nothing demo-
/// created outlives disable.
async fn sweep_orphans(state: &AppState) {
    let orphans: Vec<String> = sqlx::query_scalar(
        "SELECT p.id FROM projects p \
         WHERE p.demo_parent_id IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM demo_config c WHERE c.project_id = p.demo_parent_id)",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for id in orphans {
        tracing::info!(project = %id, "demo janitor: orphaned instance reaped");
        crate::demo::teardown_instance(state, &id).await;
    }
}

/// Self-heal the edit + shared instances (created on enable, recreated here
/// if deleted out-of-band).
async fn ensure_long_lived(state: &AppState) {
    let parents: Vec<String> = sqlx::query_scalar("SELECT project_id FROM demo_config").fetch_all(&state.db).await.unwrap_or_default();
    for parent_id in parents {
        let Ok(Some(mut config)) = crate::demo::get_config(&state.db, &parent_id).await else { continue };
        let Ok(Some(parent)) = sqlx::query_as::<_, litebin_common::types::Project>("SELECT * FROM projects WHERE id = ?")
            .bind(&parent_id)
            .fetch_optional(&state.db)
            .await
        else {
            continue;
        };
        if let Err(e) = crate::demo::ensure_instances(state, &mut config, &parent).await {
            tracing::warn!(project = %parent_id, error = %e, "demo janitor: instance ensure failed");
        }
    }
}

/// Mode B prewarm: keep `prewarm` unclaimed instances ready per demo parent
/// (running preferred; a stopped prewarm still avoids the creation path).
/// One spawn per parent per tick — refills are never urgent.
async fn prewarm_refill(state: &AppState) {
    let parents: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT c.project_id, c.prewarm, c.max_instances FROM demo_config c \
         JOIN projects p ON p.id = c.project_id WHERE c.mode = 'session' AND c.prewarm > 0",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for (parent_id, prewarm, max_instances) in parents {
        let unclaimed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM projects p \
             WHERE p.demo_parent_id = ? AND p.status IN ('running', 'stopped', 'importing') \
             AND p.id NOT IN (SELECT edit_instance_id FROM demo_config WHERE project_id = ? \
                               UNION SELECT shared_instance_id FROM demo_config WHERE project_id = ?) \
             AND NOT EXISTS (SELECT 1 FROM demo_sessions s WHERE s.instance_project_id = p.id AND s.status = 'active')",
        )
        .bind(&parent_id)
        .bind(&parent_id)
        .bind(&parent_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM projects p WHERE p.demo_parent_id = ? \
             AND p.id NOT IN (SELECT edit_instance_id FROM demo_config WHERE project_id = ? \
                               UNION SELECT shared_instance_id FROM demo_config WHERE project_id = ?)",
        )
        .bind(&parent_id)
        .bind(&parent_id)
        .bind(&parent_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if unclaimed < prewarm && total < max_instances {
            let parent: Option<litebin_common::types::Project> =
                sqlx::query_as("SELECT * FROM projects WHERE id = ?").bind(&parent_id).fetch_optional(&state.db).await.ok().flatten();
            let Some(parent) = parent else { continue };
            let baseline = crate::demo::baseline::load(state, &parent_id).await.ok();
            tracing::info!(project = %parent_id, "demo janitor: prewarm refill");
            let _ = crate::demo::clone::create_instance(
                state,
                &parent,
                crate::demo::clone::CloneOpts { start: true, baseline, requested_id: None },
            )
            .await;
        }
    }
}

/// Mode A: `reset_interval_minutes` elapsed since `last_reset_at` (or never
/// reset) → trigger a reset now. The reset handler stamps `last_reset_at`
/// even on failure, so a failing demo can't spin the loop.
async fn scheduled_resets(state: &AppState) -> anyhow::Result<()> {
    let due: Vec<(String,)> = sqlx::query_as(
        "SELECT c.project_id FROM demo_config c \
         JOIN projects p ON p.id = c.project_id \
         WHERE c.mode = 'shared' AND c.reset_interval_minutes IS NOT NULL \
         AND (c.last_reset_at IS NULL OR c.last_reset_at + c.reset_interval_minutes * 60 <= ?) \
         AND p.status NOT IN ('deploying', 'migrating', 'exporting', 'stopping', 'importing')",
    )
    .bind(chrono::Utc::now().timestamp())
    .fetch_all(&state.db)
    .await?;

    for (project_id,) in due {
        tracing::info!(project = %project_id, "demo janitor: scheduled reset");
        if let Err(e) = demo::reset::reset_project(state, &project_id).await {
            tracing::warn!(project = %project_id, error = %e, "demo janitor: scheduled reset skipped");
            // Stamp anyway so a stuck project doesn't retry every tick.
            let _ = sqlx::query("UPDATE demo_config SET last_reset_at = ? WHERE project_id = ?")
                .bind(chrono::Utc::now().timestamp())
                .bind(&project_id)
                .execute(&state.db)
                .await;
        }
    }
    Ok(())
}

/// Mode B: hard TTL. Active sessions past `expires_at` flip to `expired` and
/// their instance projects are deleted through the normal path (the instance
/// FK cascade removes the session row).
async fn expire_sessions(state: &AppState) {
    let expired: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, instance_project_id FROM demo_sessions WHERE status = 'active' AND expires_at <= ?",
    )
    .bind(chrono::Utc::now().timestamp())
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for (session_id, instance_id) in expired {
        let _ = sqlx::query("UPDATE demo_sessions SET status = 'expired' WHERE id = ?")
            .bind(&session_id)
            .execute(&state.db)
            .await;
        tracing::info!(session = %session_id, project = %instance_id, "demo janitor: session expired");
        demo::teardown_instance(state, &instance_id).await;
    }
}
