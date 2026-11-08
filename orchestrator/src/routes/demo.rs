//! Demo mode API — per-project demo config, reset, baseline, sessions.
//!
//! All handlers sit behind manage-scope auth. The visitor-facing surface
//! (gatekeeper, fringe pages, `/__demo__/*`) is NOT here — it ships with the
//! waker intercept.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use crate::demo;
use crate::AppState;
use litebin_common::types::Project;

fn err(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn load_project(state: &AppState, id: &str) -> Result<Project, axum::response::Response> {
    sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("project '{id}' not found")))
}

/// GET /projects/:id/demo — demo config + live state (baseline, sessions).
#[utoipa::path(
    get,
    path = "/projects/{id}/demo",
    params(("id" = String, Path, description = "Project ID")),
    responses((status = 200, body = serde_json::Value)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn get_demo(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let Ok(config) = demo::get_config(&state.db, &project_id).await else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
    };
    let Some(config) = config else {
        return (StatusCode::OK, Json(json!({ "enabled": false }))).into_response();
    };
    let baseline = demo::baseline::load(&state, &project_id).await.ok();
    let active_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM demo_sessions WHERE parent_project_id = ? AND status = 'active'")
            .bind(&project_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
    let instances: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE demo_parent_id = ?")
        .bind(&project_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    (
        StatusCode::OK,
        Json(json!({
            "enabled": true,
            "config": config,
            "baseline_captured": baseline.as_ref().map(|b| !b.volumes.is_empty() || b.project_tar.is_some()).unwrap_or(false),
            "active_sessions": active_sessions,
            "instances": instances,
        })),
    )
        .into_response()
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DemoConfigRequest {
    /// Enable (or disable) demo mode.
    pub enabled: bool,
    /// 'shared' or 'session'.
    pub mode: Option<String>,
    pub max_instances: Option<i64>,
    pub session_ttl_minutes: Option<i64>,
    pub prewarm: Option<i64>,
    /// Shared mode: reset interval; None = manual resets only.
    pub reset_interval_minutes: Option<i64>,
    pub banner_text: Option<String>,
    pub bar_enabled: Option<bool>,
}

/// PUT /projects/:id/demo — enable/update/disable demo mode.
#[utoipa::path(
    put,
    path = "/projects/{id}/demo",
    request_body = DemoConfigRequest,
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 400, description = "Background projects cannot be demos; bad mode"),
        (status = 404, description = "Project not found"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn put_demo(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<DemoConfigRequest>,
) -> axum::response::Response {
    let project = match load_project(&state, &project_id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    if !payload.enabled {
        if let Err(e) = disable_demo(&state, &project).await {
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("disable failed: {e}"));
        }
        return (StatusCode::OK, Json(json!({ "enabled": false }))).into_response();
    }

    if project.is_background {
        return err(StatusCode::BAD_REQUEST, "background projects have no public URL to demo".into());
    }
    let mode = payload.mode.as_deref().unwrap_or("shared");
    if !matches!(mode, "shared" | "session") {
        return err(StatusCode::BAD_REQUEST, "mode must be 'shared' or 'session'".into());
    }
    let mode_change = demo::get_config(&state.db, &project_id)
        .await
        .ok()
        .flatten()
        .is_none_or(|c| c.mode != mode);

    let now = chrono::Utc::now().timestamp();
    let updated = sqlx::query(
        "INSERT INTO demo_config (project_id, mode, max_instances, session_ttl_minutes, prewarm, \
         reset_interval_minutes, banner_text, bar_enabled, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode, \
         max_instances = excluded.max_instances, session_ttl_minutes = excluded.session_ttl_minutes, \
         prewarm = excluded.prewarm, reset_interval_minutes = excluded.reset_interval_minutes, \
         banner_text = excluded.banner_text, bar_enabled = excluded.bar_enabled, updated_at = excluded.updated_at",
    )
    .bind(&project_id)
    .bind(mode)
    .bind(payload.max_instances.unwrap_or(5).clamp(1, 50))
    .bind(payload.session_ttl_minutes.unwrap_or(15).clamp(1, 24 * 60))
    .bind(payload.prewarm.unwrap_or(1).clamp(0, 10))
    .bind(payload.reset_interval_minutes.filter(|m| *m >= 1))
    .bind(payload.banner_text.as_deref().unwrap_or("Demo environment — data resets periodically"))
    .bind(payload.bar_enabled.unwrap_or(true))
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(e) = updated {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}"));
    }

    // Seamless enable: snapshot the parent first (visitors see the app as of
    // now), then provision the instances seeded from that snapshot. The
    // janitor self-heals them every tick thereafter.
    {
        let first_enable = demo::get_config(&state.db, &project_id)
            .await
            .ok()
            .flatten()
            .is_none_or(|c| c.baseline_captured_at.is_none());
        let state_bg = state.clone();
        let parent = project.clone();
        tokio::spawn(async move {
            if first_enable && let Err(e) = demo::baseline::capture(&state_bg, &parent).await {
                tracing::warn!(project = %parent.id, error = %e, "demo: enable snapshot failed — instances start fresh");
            }
            // ensure_instances loads the (now-stamped) baseline itself and
            // seeds edit/shared from it.
            if let Ok(Some(mut cfg)) = demo::get_config(&state_bg.db, &parent.id).await {
                if let Err(e) = demo::ensure_instances(&state_bg, &mut cfg, &parent).await {
                    tracing::warn!(project = %parent.id, error = %e, "demo: instance provisioning failed");
                }
            }
        });
    }

    // Switching modes tears down the other mode's sessions.
    if mode_change && mode == "shared" {
        end_sessions(&state, &project_id).await;
    }

    tracing::info!(project = %project_id, mode, "demo mode enabled/updated");
    let _ = state.route_sync_tx.send(()); // re-route the demo host through the gatekeeper
    (StatusCode::OK, Json(json!({ "enabled": true, "mode": mode }))).into_response()
}

/// POST /projects/:id/demo/reset — reset the shared demo now (Mode A).
#[utoipa::path(
    post,
    path = "/projects/{id}/demo/reset",
    params(("id" = String, Path, description = "Project ID")),
    responses((status = 202, body = serde_json::Value), (status = 409, description = "Mid-operation")),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn reset_demo(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let project = match load_project(&state, &project_id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let Some(config) = demo::get_config(&state.db, &project_id).await.ok().flatten() else {
        return err(StatusCode::BAD_REQUEST, "demo mode is not enabled".into());
    };
    if config.mode != "shared" {
        return err(StatusCode::BAD_REQUEST, "manual reset applies to shared mode — session instances reset by expiring".into());
    }
    if demo::mid_operation(&project.status) {
        return err(StatusCode::CONFLICT, format!("project is {} — wait for it to settle", project.status));
    }
    match demo::reset::reset_project(&state, &project_id).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({ "status": "resetting", "restores_baseline": config.baseline_captured_at.is_some() })),
        )
            .into_response(),
        Err(e) => err(StatusCode::CONFLICT, e.to_string()),
    }
}

/// POST /projects/:id/demo/baseline — freeze the base's current state.
#[utoipa::path(
    post,
    path = "/projects/{id}/demo/baseline",
    params(("id" = String, Path, description = "Project ID")),
    responses((status = 202, body = serde_json::Value), (status = 409, description = "Mid-operation")),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn capture_baseline(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let project = match load_project(&state, &project_id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if demo::get_config(&state.db, &project_id).await.ok().flatten().is_none() {
        return err(StatusCode::BAD_REQUEST, "enable demo mode first".into());
    }
    if demo::mid_operation(&project.status) {
        return err(StatusCode::CONFLICT, format!("project is {} — wait for it to settle", project.status));
    }
    let state_bg = state.clone();
    let id = project_id.clone();
    tokio::spawn(async move {
        match demo::baseline::capture(&state_bg, &project).await {
            Ok(()) => {
                // Apply the new seed to what's already running.
                propagate_seed_change(&state_bg, &id).await;
            }
            Err(e) => tracing::error!(project = %id, error = %e, "baseline capture failed"),
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "status": "capturing" }))).into_response()
}

/// DELETE /projects/:id/demo/baseline — remove the seed (revert to image-only).
#[utoipa::path(
    delete,
    path = "/projects/{id}/demo/baseline",
    params(("id" = String, Path, description = "Project ID")),
    responses((status = 200, body = serde_json::Value)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn delete_baseline(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    let project = match load_project(&state, &project_id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match demo::baseline::remove(&state, &project).await {
        Ok(()) => {
            propagate_seed_change(&state, &project_id).await;
            // The edit copy holds the discarded edits — wipe it to a clean
            // boot so the next publish can't resurrect them.
            if let Some(edit_id) =
                demo::get_config(&state.db, &project_id).await.ok().flatten().and_then(|c| c.edit_instance_id)
            {
                if let Err(e) = demo::reset::wipe_instance(&state, &edit_id).await {
                    tracing::warn!(project = %project_id, error = %e, "demo: edit-copy wipe failed");
                }
            }
            (StatusCode::OK, Json(json!({ "status": "removed" }))).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// GET /projects/:id/demo/sessions — live + ended sessions with instance state.
#[utoipa::path(
    get,
    path = "/projects/{id}/demo/sessions",
    params(("id" = String, Path, description = "Project ID")),
    responses((status = 200, body = serde_json::Value)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    #[derive(sqlx::FromRow, serde::Serialize)]
    struct SessionRow {
        id: String,
        instance_project_id: String,
        created_at: i64,
        expires_at: i64,
        last_seen_at: i64,
        status: String,
        instance_status: Option<String>,
    }
    let rows: Vec<SessionRow> = sqlx::query_as(
        "SELECT s.id, s.instance_project_id, s.created_at, s.expires_at, s.last_seen_at, s.status, \
         p.status AS instance_status \
         FROM demo_sessions s LEFT JOIN projects p ON p.id = s.instance_project_id \
         WHERE s.parent_project_id = ? ORDER BY s.created_at DESC LIMIT 100",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Long-lived instances (edit + shared) — visible and stoppable, never deleted here.
    let mut long_lived: Vec<serde_json::Value> = Vec::new();
    if let Ok(Some(config)) = demo::get_config(&state.db, &project_id).await {
        for (kind, id) in [
            ("edit", config.edit_instance_id.as_deref()),
            ("shared", config.shared_instance_id.as_deref()),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        {
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
                .bind(id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
            long_lived.push(json!({ "id": id, "kind": kind, "status": status }));
        }
    }

    (StatusCode::OK, Json(json!({ "sessions": rows, "long_lived": long_lived }))).into_response()
}

/// DELETE /projects/:id/demo/sessions/:sid — kill one session + its instance.
#[utoipa::path(
    delete,
    path = "/projects/{id}/demo/sessions/{sid}",
    params(("id" = String, Path), ("sid" = String, Path, description = "Session ID")),
    responses((status = 200, body = serde_json::Value), (status = 404, description = "Session not found")),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn kill_session(
    State(state): State<AppState>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> axum::response::Response {
    let instance: Option<String> =
        sqlx::query_scalar("SELECT instance_project_id FROM demo_sessions WHERE id = ? AND parent_project_id = ?")
            .bind(&session_id)
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
    let Some(instance) = instance else {
        return err(StatusCode::NOT_FOUND, format!("session '{session_id}' not found"));
    };
    let _ = sqlx::query("UPDATE demo_sessions SET status = 'ended' WHERE id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await;
    demo::teardown_instance(&state, &instance).await;
    (StatusCode::OK, Json(json!({ "status": "ended" }))).into_response()
}

/// A baseline capture/removal changed the demo's seed — apply it to what's
/// already running: shared mode resets the shared instance (restores or
/// clears); session mode purges unclaimed pool instances so the next claim
/// spawns from the new seed. The janitor refills the pool.
async fn propagate_seed_change(state: &AppState, parent_id: &str) {
    let Ok(Some(config)) = demo::get_config(&state.db, parent_id).await else { return };
    if config.mode == "shared" {
        if let Err(e) = demo::reset::reset_project(state, parent_id).await {
            tracing::warn!(project = %parent_id, error = %e, "demo: seed-change reset skipped");
        }
        return;
    }
    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT p.id FROM projects p \
         WHERE p.demo_parent_id = ? \
         AND p.id NOT IN (SELECT edit_instance_id FROM demo_config WHERE project_id = ? \
                           UNION SELECT shared_instance_id FROM demo_config WHERE project_id = ?) \
         AND NOT EXISTS (SELECT 1 FROM demo_sessions s WHERE s.instance_project_id = p.id AND s.status = 'active')",
    )
    .bind(parent_id)
    .bind(parent_id)
    .bind(parent_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for id in stale {
        tracing::info!(project = %parent_id, instance = %id, "demo: purging stale-seed pool instance");
        demo::teardown_instance(state, &id).await;
    }
}

/// Disable: config off immediately (instant toggle), full cleanup in the
/// background. The parent is untouched and serves normally again.
async fn disable_demo(state: &AppState, project: &Project) -> anyhow::Result<()> {
    demo::disable(state, &project.id).await;
    let _ = state.route_sync_tx.send(());
    tracing::info!(project = %project.id, "demo mode disabled (cleanup in background)");
    Ok(())
}

async fn end_sessions(state: &AppState, parent_id: &str) {
    let instances: Vec<String> = sqlx::query_scalar(
        "SELECT instance_project_id FROM demo_sessions WHERE parent_project_id = ? AND status = 'active'",
    )
    .bind(parent_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let _ = sqlx::query("UPDATE demo_sessions SET status = 'ended' WHERE parent_project_id = ?")
        .bind(parent_id)
        .execute(&state.db)
        .await;
    for instance in instances {
        demo::teardown_instance(state, &instance).await;
    }
}
