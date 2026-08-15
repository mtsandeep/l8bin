use std::collections::HashMap;
use std::sync::Arc;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::AppState;
use litebin_common::types::{DeployType, ProjectStatus};

use super::form::ComposeForm;
use super::preflight::{TargetPreflightError, require_live_host_network_target, resolve_target_before_mutation};
use super::validate::ValidatedCompose;

/// State persisted by the deploy, needed by the stage/remote/local phases.
pub(super) struct PersistedCompose {
    pub project: crate::db::models::Project,
    pub docker_observe: bool,
    pub host_network: bool,
    pub target_node_id: String,
    pub old_service_digests: HashMap<String, String>,
    pub existing_node_id: Option<String>,
}

/// Acquire the deploy lock, resolve + preflight the target node, capture old
/// digests, write compose.yaml, upsert the projects/services/volumes rows,
/// persist grants, and read the project back.
pub(super) async fn persist_compose_deploy(
    state: &AppState,
    user_id: &str,
    form: &ComposeForm,
    v: &ValidatedCompose,
    now: i64,
) -> Result<PersistedCompose, axum::response::Response> {
    let project_id = &form.project_id;
    let compose = &v.compose;

    // Acquire deploy lock
    let semaphore =
        state.project_locks.entry(project_id.clone()).or_insert_with(|| Arc::new(Semaphore::new(1))).clone();
    let _permit = semaphore.acquire().await.unwrap();

    // Re-read the sticky node under the deploy lock so a queued redeploy sees
    // the target selected by the deploy that ran immediately before it.
    let existing_node_id: Option<String> = sqlx::query_scalar("SELECT node_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    // Resolve the sticky/override/automatic target and validate host-network
    // eligibility before changing project state, grants, artifacts, or stage metadata.
    let requests_host_network = compose.services.values().any(|service| service.uses_host_network());
    let preflight_state = state;
    let target_node_id = match resolve_target_before_mutation(
        &state.db,
        existing_node_id.as_deref(),
        form.node_id.clone(),
        requests_host_network,
        |target_node_id| async move { require_live_host_network_target(preflight_state, &target_node_id).await },
    )
    .await
    {
        Ok(id) => id,
        Err(TargetPreflightError::Selection(error)) => {
            return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": format!("{error:?}")}))).into_response());
        }
        Err(TargetPreflightError::Eligibility(error)) => {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": error.to_string()}))).into_response());
        }
    };

    // Capture old per-service image digests for cleanup after redeploy.
    let old_service_digests = if v.is_update {
        crate::routes::manage::capture_service_digests(state, project_id, existing_node_id.as_deref(), None).await
    } else {
        std::collections::HashMap::new()
    };

    // Ensure project directory exists and write compose.yaml to disk
    crate::routes::manage::ensure_project_dir_and_env(project_id);

    let compose_path = std::path::PathBuf::from("projects").join(project_id).join("compose.yaml");
    if let Err(e) = std::fs::write(&compose_path, &form.compose_yaml) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to write compose.yaml: {e}")})),
        )
            .into_response());
    }

    // Determine the public service's port for the projects row
    let public_svc = v.public_service.as_deref().map(|name| &compose.services[name]);
    let public_port: Option<i64> = public_svc.and_then(|svc| svc.exposed_ports().first().map(|(p, _)| *p as i64));
    let public_image: Option<String> = public_svc.and_then(|svc| svc.image.clone());

    // Build service_count and service_summary
    let service_count = compose.services.len() as i64;
    let service_summary = v.start_order.join(":");

    // On partial redeploy, project stays running (we're only updating a subset of services).
    // First-deploy staging remains pending until artifacts and runtime config are ready.
    let project_status = if v.stage_only {
        ProjectStatus::Pending
    } else if v.target_services.is_some() {
        ProjectStatus::Running
    } else {
        ProjectStatus::Deploying
    };

    // On redeploy, preserve existing raw-port access unless explicitly provided.
    let db_allow_raw_ports = if v.is_update && form.allow_raw_ports.is_none() {
        let existing = match sqlx::query_scalar::<_, bool>("SELECT allow_raw_ports FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to read allow_raw_ports, using defaults");
                None
            }
        };
        existing.or(form.allow_raw_ports)
    } else {
        form.allow_raw_ports
    };

    let allow_raw_ports = db_allow_raw_ports.unwrap_or(false);

    // Upsert project row
    let result = sqlx::query(
        r#"
        INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, status, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, custom_domain, allow_raw_ports, allow_docker_access, service_count, service_summary, deploy_type, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            user_id = excluded.user_id,
            is_background = excluded.is_background,
            image = excluded.image,
            internal_port = excluded.internal_port,
            container_id = CASE WHEN excluded.is_background = 1 THEN NULL ELSE projects.container_id END,
            mapped_port = CASE WHEN excluded.is_background = 1 THEN NULL ELSE projects.mapped_port END,
            status = CASE WHEN excluded.status = 'running' THEN projects.status ELSE excluded.status END,
            name = CASE WHEN excluded.name IS NOT NULL THEN excluded.name ELSE COALESCE(projects.name, excluded.name) END,
            description = CASE WHEN excluded.description IS NOT NULL THEN excluded.description ELSE COALESCE(projects.description, excluded.description) END,
            auto_stop_enabled = excluded.auto_stop_enabled,
            auto_stop_timeout_mins = excluded.auto_stop_timeout_mins,
            auto_start_enabled = excluded.auto_start_enabled,
            custom_domain = CASE WHEN excluded.custom_domain IS NOT NULL THEN excluded.custom_domain ELSE COALESCE(projects.custom_domain, excluded.custom_domain) END,
            allow_raw_ports = excluded.allow_raw_ports,
            allow_docker_access = excluded.allow_docker_access,
            service_count = excluded.service_count,
            service_summary = excluded.service_summary,
            deploy_type = excluded.deploy_type,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .bind(&form.name)
    .bind(&form.description)
    .bind(v.is_background)
    .bind(&public_image)
    .bind(public_port)
    .bind(project_status)
    .bind(v.auto_stop)
    .bind(v.auto_stop_mins)
    .bind(v.auto_start)
    .bind(&form.custom_domain)
    .bind(allow_raw_ports)
    .bind(false)
    .bind(service_count)
    .bind(&service_summary)
    .bind(DeployType::Compose)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        let is_conflict = crate::validation::is_unique_constraint(&e);
        let status = if is_conflict { StatusCode::CONFLICT } else { StatusCode::INTERNAL_SERVER_ERROR };
        return Err((
            status,
            Json(json!({"error": if is_conflict { format!("project '{}' already exists", project_id) } else { format!("database error: {e}") } })),
        ).into_response());
    }

    // Persist any newly approved capabilities (syncs legacy allow_* columns).
    if let Err(e) = crate::capabilities::grant_many(&state.db, project_id, &v.pending_grants, Some(user_id)).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to grant capabilities: {e}")})),
        )
            .into_response());
    }

    // Read project back from DB
    let project = match sqlx::query_as::<_, crate::db::models::Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("database error: {e}")})))
                .into_response());
        }
    };
    let docker_observe = match crate::capabilities::has_capability(
        &state.db,
        project_id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    {
        Ok(granted) => granted,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to read docker-observe grant: {e}")})),
            )
                .into_response());
        }
    };
    let host_network = match crate::capabilities::has_capability(
        &state.db,
        project_id,
        litebin_common::capabilities::ProjectCapability::HostNetwork,
    )
    .await
    {
        Ok(granted) => granted,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to read host-network grant: {e}")})),
            )
                .into_response());
        }
    };

    // Seed project_services rows for each service in the compose file
    let target_set: Option<std::collections::HashSet<String>> =
        v.target_services.as_ref().map(|ts| ts.iter().cloned().collect());
    let oneshot_names = compose.oneshot_service_names();
    for svc_name in &v.start_order {
        let svc = &compose.services[svc_name];
        let image = svc.image.clone().unwrap_or_default();
        let port: Option<i64> = svc
            .ports
            .as_ref()
            .and_then(|p| p.first())
            .and_then(|spec| litebin_common::compose_run::container_port(spec))
            .map(|p: u16| p as i64);
        let is_public = !v.is_background && v.public_service.as_deref() == Some(svc_name.as_str());
        let is_oneshot = oneshot_names.contains(svc_name);
        let depends_on = svc.depends_on.as_ref().and_then(|d| serde_json::to_string(d).ok());
        let compose_mem: Option<i64> = svc.memory_bytes().map(|bytes| (bytes / (1024 * 1024)) as i64);
        let compose_cpu: Option<f64> =
            svc.cpus.as_ref().and_then(|val| val.as_f64().or_else(|| val.as_str().and_then(|s| s.parse::<f64>().ok())));

        // On redeploy, preserve DB overrides when compose file doesn't specify memory/CPU
        let existing_override: Option<(Option<i64>, Option<f64>)> = sqlx::query_as(
            "SELECT memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ? AND service_name = ?",
        )
        .bind(project_id)
        .bind(svc_name)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let memory_limit_mb = compose_mem.or_else(|| existing_override.as_ref().and_then(|(m, _)| *m));
        let cpu_limit = compose_cpu.or_else(|| existing_override.as_ref().and_then(|(_, c)| *c));

        // On partial redeploy, only mark targeted services as 'deploying'
        let svc_status = if v.stage_only {
            ProjectStatus::Pending
        } else if target_set.as_ref().map_or(true, |ts| ts.contains(svc_name)) {
            ProjectStatus::Deploying
        } else {
            // Preserve current status for non-targeted services
            ProjectStatus::Running
        };
        if let Err(e) = sqlx::query(
            "INSERT OR REPLACE INTO project_services (project_id, service_name, image, port, is_public, depends_on, memory_limit_mb, cpu_limit, status, is_oneshot)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(svc_name)
        .bind(&image)
        .bind(port)
        .bind(is_public)
        .bind(&depends_on)
        .bind(memory_limit_mb)
        .bind(cpu_limit)
        .bind(svc_status)
        .bind(is_oneshot)
        .execute(&state.db)
        .await
        {
            tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to upsert project_services row");
        }
    }

    // Seed project_volumes rows from compose volume definitions
    if let Err(e) =
        sqlx::query("DELETE FROM project_volumes WHERE project_id = ?").bind(project_id).execute(&state.db).await
    {
        tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to delete existing volumes");
    }
    for svc_name in &v.start_order {
        let svc = &compose.services[svc_name];
        if let Some(ref vols) = svc.volumes {
            for vol_str in vols {
                // Parse "source:target[:mode]" format
                let parts: Vec<&str> = vol_str.splitn(3, ':').collect();
                if parts.len() >= 2 {
                    let volume_name = if !parts[0].is_empty() {
                        Some(litebin_common::types::scope_volume_source(parts[0], project_id))
                    } else {
                        None
                    };
                    let container_path = parts[1].to_string();
                    if let Err(e) = sqlx::query(
                        "INSERT OR IGNORE INTO project_volumes (project_id, service_name, volume_name, container_path)
                         VALUES (?, ?, ?, ?)",
                    )
                    .bind(project_id)
                    .bind(svc_name)
                    .bind(&volume_name)
                    .bind(&container_path)
                    .execute(&state.db)
                    .await
                    {
                        tracing::warn!(project_id = %project_id, service = %svc_name, error = %e, "compose deploy: failed to insert volume row");
                    }
                }
            }
        }
    }

    // Persist sticky node selection even for staged first deploys.
    if let Err(e) = sqlx::query("UPDATE projects SET node_id = ?, updated_at = ? WHERE id = ?")
        .bind(&target_node_id)
        .bind(now)
        .bind(project_id)
        .execute(&state.db)
        .await
    {
        tracing::warn!(project_id = %project_id, error = %e, "compose deploy: failed to persist node_id");
    }

    Ok(PersistedCompose {
        project,
        docker_observe,
        host_network,
        target_node_id,
        old_service_digests,
        existing_node_id,
    })
}
