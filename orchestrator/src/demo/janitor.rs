//! Demo janitor — the demo lifecycle loop (a sibling of the scale-to-zero
//! janitor). Every tick it:
//!
//! 1. Runs scheduled Mode A resets whose interval elapsed.
//! 2. Expires Mode B sessions past their hard TTL and tears down their
//!    instance projects (in-flight connections die with the containers —
//!    hard kill by design, no drain grace).
//!
//! Prewarm refills and orphan-instance reconciliation land with the session
//! pool (gatekeeper step); the loop is started unconditionally so the reset
//! scheduler works the moment Mode A ships.

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
    Ok(())
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
