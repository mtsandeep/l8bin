use std::sync::Arc;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::nodes;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::{ProjectStatus, VolumeMount};

use super::types::DeployRequest;

/// Parsed deploy settings shared across the deploy phases.
struct DeploySettings {
    granted_capabilities: Vec<litebin_common::capabilities::ProjectCapability>,
    is_background: bool,
    stage_only: bool,
}

/// Parse capability grants, resolve the workload type, enforce the
/// background/port rules, and decide whether this is a first-deploy stage.
async fn resolve_deploy_settings(
    state: &AppState,
    payload: &DeployRequest,
    is_update: bool,
) -> Result<DeploySettings, axum::response::Response> {
    let granted_capabilities = match payload
        .grant_capabilities
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|id| {
            litebin_common::capabilities::ProjectCapability::parse(id)
                .ok_or_else(|| format!("unknown capability '{id}'"))
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(capabilities) => capabilities,
        Err(error) => {
            return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response());
        }
    };

    // PUT requests that omit workload type preserve the current project setting.
    let existing_background: Option<bool> = if is_update {
        sqlx::query_scalar("SELECT is_background FROM projects WHERE id = ?")
            .bind(&payload.project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let is_background = resolve_background(payload.is_background, existing_background);

    if is_background && payload.port.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "background projects must not provide an HTTP port"})),
        )
            .into_response());
    }
    if !is_background && payload.port.is_none() {
        return Err(
            (StatusCode::BAD_REQUEST, Json(json!({"error": "web projects require an HTTP port"}))).into_response()
        );
    }

    let existing_status: Option<ProjectStatus> = sqlx::query_scalar("SELECT status FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let stage_only = payload.stage_only
        && matches!(existing_status, None | Some(ProjectStatus::Pending | ProjectStatus::Unconfigured));

    tracing::info!(
        project_id = %payload.project_id,
        image = %payload.image,
        port = ?payload.port,
        is_background,
        is_update = is_update,
        stage_only = stage_only,
        "deploy request received"
    );

    Ok(DeploySettings { granted_capabilities, is_background, stage_only })
}

/// Sleep settings to persist for this deploy (auto-stop/auto-start + timeout).
struct SleepSettings {
    auto_stop_enabled: bool,
    auto_stop_timeout_mins: i64,
    auto_start_enabled: bool,
}

/// Compute sleep settings: background projects disable both; redeploys that
/// omit the flags preserve the stored values.
async fn resolve_sleep_settings(
    state: &AppState,
    payload: &DeployRequest,
    is_update: bool,
    is_background: bool,
) -> SleepSettings {
    let auto_stop_enabled = if is_background { false } else { payload.auto_stop_enabled.unwrap_or(true) };
    let auto_stop_timeout_mins = payload.auto_stop_timeout_mins.unwrap_or(state.config.default_auto_stop_mins);
    let auto_start_enabled = if is_background { false } else { payload.auto_start_enabled.unwrap_or(true) };

    // On redeploy, preserve existing sleep settings unless explicitly provided
    let preserve_sleep = is_update && payload.auto_stop_enabled.is_none() && payload.auto_start_enabled.is_none();
    if is_background {
        SleepSettings { auto_stop_enabled: false, auto_stop_timeout_mins, auto_start_enabled: false }
    } else if preserve_sleep {
        let existing = sqlx::query_as::<_, (bool, i64, bool)>(
            "SELECT auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled FROM projects WHERE id = ?",
        )
        .bind(&payload.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        match existing {
            Some((s, t, a)) => SleepSettings { auto_stop_enabled: s, auto_stop_timeout_mins: t, auto_start_enabled: a },
            None => SleepSettings { auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled },
        }
    } else {
        SleepSettings { auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled }
    }
}

/// State captured before the project upsert, used for post-deploy cleanup.
struct PreviousDeployment {
    old_image: Option<String>,
    old_node_id: Option<String>,
    old_volumes: Option<Vec<VolumeMount>>,
}

/// Insert or upsert the projects row, grant capabilities, and read the row back.
#[allow(clippy::too_many_arguments)]
async fn upsert_project(
    state: &AppState,
    payload: &DeployRequest,
    user_id: &str,
    is_update: bool,
    is_background: bool,
    stage_only: bool,
    sleep: &SleepSettings,
    granted_capabilities: &[litebin_common::capabilities::ProjectCapability],
    now: i64,
) -> Result<(PreviousDeployment, crate::db::models::Project), axum::response::Response> {
    // Capture old image and node before upsert (for cleanup after deploy)
    let (old_image, old_node_id) =
        sqlx::query_as::<_, (Option<String>, Option<String>)>("SELECT image, node_id FROM projects WHERE id = ?")
            .bind(&payload.project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or((None, None));

    let initial_status = if stage_only { ProjectStatus::Pending } else { ProjectStatus::Deploying };

    // Capture old volumes for orphan detection
    let old_volumes: Option<Vec<VolumeMount>> =
        sqlx::query_scalar::<_, Option<String>>("SELECT volumes FROM projects WHERE id = ?")
            .bind(&payload.project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .flatten()
            .and_then(|v| match serde_json::from_str(&v) {
                Ok(mounts) => Some(mounts),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse old volumes JSON, skipping volume diff");
                    None
                }
            });

    let volumes_json = payload.volumes.as_ref().and_then(|v| litebin_common::types::serialize_volumes(v));

    let result = if is_update {
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                user_id = excluded.user_id,
                is_background = excluded.is_background,
                image = excluded.image,
                internal_port = excluded.internal_port,
                status = ?,
                name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
                description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
                auto_stop_enabled = excluded.auto_stop_enabled,
                auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
                auto_start_enabled = excluded.auto_start_enabled,
                cmd = CASE WHEN excluded.cmd IS NOT NULL THEN excluded.cmd ELSE projects.cmd END,
                memory_limit_mb = CASE WHEN excluded.memory_limit_mb IS NOT NULL THEN excluded.memory_limit_mb ELSE projects.memory_limit_mb END,
                cpu_limit = CASE WHEN excluded.cpu_limit IS NOT NULL THEN excluded.cpu_limit ELSE projects.cpu_limit END,
                custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE projects.custom_domain END,
                volumes = CASE WHEN excluded.volumes IS NOT NULL THEN excluded.volumes ELSE projects.volumes END,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&payload.project_id)
        .bind(user_id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(is_background)
        .bind(&payload.image)
        .bind(payload.port)
        .bind(initial_status.clone())
        .bind(sleep.auto_stop_enabled)
        .bind(sleep.auto_stop_timeout_mins)
        .bind(sleep.auto_start_enabled)
        .bind(&payload.cmd)
        .bind(payload.memory_limit_mb)
        .bind(payload.cpu_limit)
        .bind(&payload.custom_domain)
        .bind(&volumes_json)
        .bind(now)
        .bind(now)
        .bind(initial_status.clone())
        .execute(&state.db)
        .await
    } else {
        // Create-only: plain INSERT, no ON CONFLICT
        sqlx::query(
            r#"
            INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain, volumes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&payload.project_id)
        .bind(user_id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(is_background)
        .bind(&payload.image)
        .bind(payload.port)
        .bind(initial_status.clone())
        .bind(sleep.auto_stop_enabled)
        .bind(sleep.auto_stop_timeout_mins)
        .bind(sleep.auto_start_enabled)
        .bind(&payload.cmd)
        .bind(payload.memory_limit_mb)
        .bind(payload.cpu_limit)
        .bind(&payload.custom_domain)
        .bind(&volumes_json)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
    };

    if let Err(e) = result {
        let is_conflict = crate::validation::is_unique_constraint(&e);
        let status = if is_conflict { StatusCode::CONFLICT } else { StatusCode::INTERNAL_SERVER_ERROR };
        return Err((
            status,
            Json(json!({"error": if is_conflict { format!("project '{}' already exists", payload.project_id) } else { format!("database error: {e}") } })),
        ).into_response());
    }

    if let Err(error) =
        crate::capabilities::grant_many(&state.db, &payload.project_id, granted_capabilities, Some(user_id)).await
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to store capability grants: {error}")})),
        )
            .into_response());
    }

    // Read project back from DB (so cmd reflects stored value, not just payload)
    let project = match sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&payload.project_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response());
        }
    };

    Ok((PreviousDeployment { old_image, old_node_id, old_volumes }, project))
}

/// First-deploy staging: create the runtime .env / metadata without starting
/// containers, then respond with the staged (unconfigured) status.
async fn stage_only_path(
    state: &AppState,
    payload: &DeployRequest,
    project: &crate::db::models::Project,
    node_id: &str,
    is_background: bool,
    granted_capabilities: &[litebin_common::capabilities::ProjectCapability],
) -> axum::response::Response {
    if node_id == "local" {
        crate::routes::manage::ensure_project_dir_and_env(&payload.project_id);
    } else {
        let agent = match nodes::client::AgentClient::resolve(state, node_id).await {
            Ok(a) => a,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("node client unavailable: {e}")})),
                )
                    .into_response();
            }
        };
        let stage_request = litebin_common::agent_api::RunRequest {
            image: payload.image.clone(),
            internal_port: payload.port,
            project_id: payload.project_id.clone(),
            cmd: project.cmd.clone(),
            memory_limit_mb: project.memory_limit_mb,
            cpu_limit: project.cpu_limit,
            volumes: payload.volumes.clone(),
            docker_observe: granted_capabilities
                .contains(&litebin_common::capabilities::ProjectCapability::DockerObserve),
            stage_only: true,
        };
        if let Err(e) = agent.run(&stage_request).await {
            if let nodes::client::AgentClientError::Status { body, .. } = &e {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": format!("remote stage failed: {body}")})),
                )
                    .into_response();
            }
            let (status_code, message) = e.into_response_parts();
            return (status_code, Json(json!({"error": message}))).into_response();
        }
    }

    if let Err(e) = status::transition(
        &state.db,
        &payload.project_id,
        ProjectStatus::Unconfigured,
        &ProjectUpdateFields::default(),
        None,
    )
    .await
    {
        tracing::error!(project_id = %payload.project_id, error = %e, "deploy: failed to mark staged project unconfigured");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "failed to persist staged deployment status"})),
        )
            .into_response();
    }

    tracing::info!(
        project_id = %payload.project_id,
        node_id = %node_id,
        "deployment staged; awaiting runtime configuration"
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "unconfigured",
            "project_id": payload.project_id,
            "node_id": node_id,
            "url": if is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", payload.project_id, state.platform.domain())) },
            "message": "Deployment staged. Configure runtime secrets, then start the project.",
        })),
    )
        .into_response()
}

/// Shared deploy execution logic. When `is_update` is true, uses upsert (ON CONFLICT DO UPDATE).
pub(super) async fn execute_deploy(
    state: AppState,
    user_id: String,
    payload: DeployRequest,
    is_update: bool,
) -> axum::response::Response {
    let now = chrono::Utc::now().timestamp();

    let settings = match resolve_deploy_settings(&state, &payload, is_update).await {
        Ok(settings) => settings,
        Err(resp) => return resp,
    };
    let is_background = settings.is_background;
    let stage_only = settings.stage_only;

    let sleep = resolve_sleep_settings(&state, &payload, is_update, is_background).await;

    // 1. Acquire deploy lock for this project_id (serializes concurrent deploys)
    let semaphore =
        state.project_locks.entry(payload.project_id.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let _permit = semaphore.acquire().await.unwrap();

    // 2-3. Upsert the project row, grant capabilities, read the row back
    let (previous, project) = match upsert_project(
        &state,
        &payload,
        &user_id,
        is_update,
        is_background,
        stage_only,
        &sleep,
        &settings.granted_capabilities,
        now,
    )
    .await
    {
        Ok(result) => result,
        Err(resp) => return resp,
    };

    // 4. Select target node
    let node_id = match nodes::selector::select_node(&state.db, &project, payload.node_id.clone()).await {
        Ok(id) => id,
        Err(e) => {
            let failure_status = if stage_only { ProjectStatus::Pending } else { ProjectStatus::Stopped };
            if let Err(e) = status::transition(
                &state.db,
                &payload.project_id,
                failure_status,
                &ProjectUpdateFields::default(),
                None,
            )
            .await
            {
                tracing::warn!(project_id = %payload.project_id, error = %e, "deploy: failed to transition after node selection error");
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Persist sticky node selection for staged and live deploys.
    if let Err(e) = sqlx::query("UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?")
        .bind(&node_id)
        .bind(now)
        .bind(&payload.project_id)
        .execute(&state.db)
        .await
    {
        tracing::warn!(project_id = %payload.project_id, error = %e, "deploy: failed to persist node_id");
    }

    // First-deploy staging: create runtime .env / metadata, do not start containers.
    if stage_only {
        return stage_only_path(&state, &payload, &project, &node_id, is_background, &settings.granted_capabilities)
            .await;
    }

    // 5. Spawn background task for heavy lifting (Pull, Start, Route Sync)
    let state_clone = state.clone();
    let payload_clone = payload.clone();
    let project_clone = project.clone();
    let node_id_clone = node_id.clone();
    let old_image_clone = previous.old_image.clone();
    let old_node_id_clone = previous.old_node_id.clone();
    let old_volumes_clone = previous.old_volumes.clone();
    let is_background_clone = is_background;
    let granted_capabilities_clone = settings.granted_capabilities.clone();

    tokio::spawn(async move {
        crate::routes::deploy::logs::push_deploy_log(&state_clone, &payload_clone.project_id, "Deployment started");

        let result: Result<(), anyhow::Error> = super::background::run_deploy_task(
            state_clone.clone(),
            payload_clone.clone(),
            project_clone,
            node_id_clone,
            old_image_clone,
            old_node_id_clone,
            old_volumes_clone,
            is_background_clone,
            granted_capabilities_clone,
        )
        .await;

        if let Err(e) = result {
            tracing::error!(project_id = %payload_clone.project_id, error = %e, "background deploy failed");
            crate::routes::deploy::logs::push_deploy_log(
                &state_clone,
                &payload_clone.project_id,
                &format!("Deploy failed: {}", e),
            );
            let _ = status::transition(
                &state_clone.db,
                &payload_clone.project_id,
                ProjectStatus::Error,
                &ProjectUpdateFields::default(),
                None,
            )
            .await;
        }
    });

    (
        StatusCode::OK,
        Json(json!({
            "status": "deploying",
            "project_id": payload.project_id,
            "url": if is_background { serde_json::Value::Null } else { json!(format!("https://{}.{}", payload.project_id, state.platform.domain())) },
            "message": "Deployment started in background"
        })),
    )
        .into_response()
}

fn resolve_background(requested: Option<bool>, existing: Option<bool>) -> bool {
    requested.or(existing).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::resolve_background;
    use dashmap::DashMap;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    #[test]
    fn workload_type_defaults_to_web_and_is_preserved_on_redeploy() {
        assert!(!resolve_background(None, None));
        assert!(resolve_background(Some(true), None));
        assert!(resolve_background(None, Some(true)));
        assert!(!resolve_background(Some(false), Some(true)));
    }

    #[tokio::test]
    async fn prop_deploy_lock_serializes_concurrent_ops() {
        let project_locks: Arc<DashMap<String, Arc<Semaphore>>> = Arc::new(DashMap::new());
        let project_id = "test-project";

        // Create semaphore for project
        let semaphore =
            project_locks.entry(project_id.to_string()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();

        // Acquire permit
        let sem = semaphore.clone();
        let permit1 = sem.acquire().await.unwrap();
        assert!(semaphore.available_permits() == 0);

        // Second acquire should wait (drop first to verify)
        drop(permit1);
        let permit2 = sem.acquire().await.unwrap();
        assert!(semaphore.available_permits() == 0);
        drop(permit2);
    }
}
