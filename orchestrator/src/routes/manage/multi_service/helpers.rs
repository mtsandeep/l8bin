use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;

use crate::AppState;
use crate::status;

use crate::routes::manage::helpers::read_local_project_env;
use litebin_common::agent_api::{BatchRunRequest, ServiceResources};

pub(in crate::routes::manage) fn proxy_needed_after_stop(
    requesters: &HashSet<String>,
    running_services: &HashSet<String>,
    stopping_services: Option<&HashSet<String>>,
) -> bool {
    let Some(stopping_services) = stopping_services else {
        return false;
    };
    requesters.iter().any(|service| running_services.contains(service) && !stopping_services.contains(service))
}

pub(in crate::routes::manage) async fn approved_docker_observe_requesters(
    state: &AppState,
    project: &crate::db::models::Project,
) -> Result<HashSet<String>, (StatusCode, String)> {
    let approved = crate::capabilities::has_capability(
        &state.db,
        &project.id,
        litebin_common::capabilities::ProjectCapability::DockerObserve,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("capability lookup failed: {e}")))?;
    if !approved {
        return Ok(HashSet::new());
    }

    let extra_env = read_local_project_env(&project.id);
    let plan = if let Some(yaml) = litebin_common::docker::DockerManager::read_compose(&project.id) {
        let compose = compose_bollard::ComposeParser::parse_with_interpolation(&yaml, &extra_env, false)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("invalid stored compose.yaml: {e}")))?;
        litebin_common::compose_run::ComposeRunPlan::from_compose(&compose, &project.id, &extra_env, None)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("stored compose plan error: {e}")))?
    } else {
        litebin_common::compose_run::ComposeRunPlan::single_service(
            litebin_common::types::RunServiceConfig::from_project(project, extra_env),
        )
    };
    Ok(plan.docker_socket_requester_names())
}

pub(super) async fn mark_replacement_failure(state: &AppState, project_id: &str, services: &HashSet<String>) {
    for service in services {
        if service == litebin_common::types::DOCKER_PROXY_SERVICE {
            continue;
        }
        if let Err(error) = status::set_service_replacement_error(&state.db, project_id, service).await {
            tracing::warn!(
                project_id = %project_id,
                service = %service,
                %error,
                "failed to clear service metadata after replacement failure"
            );
        }
    }
}

fn select_reported_affected_services(reported: &[String], known_services: &HashSet<String>) -> HashSet<String> {
    reported
        .iter()
        .filter(|service| {
            service.as_str() != litebin_common::types::DOCKER_PROXY_SERVICE && known_services.contains(service.as_str())
        })
        .cloned()
        .collect()
}

pub(super) fn should_abort_siblings(rollback_on_failure: bool, proxy_created: bool) -> bool {
    rollback_on_failure || proxy_created
}

pub(super) fn cancellation_cleanup_services(
    create_attempted: &HashSet<String>,
    started: &[(String, String)],
) -> HashSet<String> {
    create_attempted.iter().cloned().chain(started.iter().map(|(service, _)| service.clone())).collect()
}

pub(crate) async fn apply_remote_batch_failure_metadata(state: &AppState, project_id: &str, response_body: &str) {
    let reported = serde_json::from_str::<litebin_common::agent_api::BatchRunErrorResponse>(response_body)
        .map(|body| body.affected_services)
        .unwrap_or_default();
    if reported.is_empty() {
        return;
    }
    let known_services =
        sqlx::query_scalar::<_, String>("SELECT service_name FROM project_services WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
    let affected = select_reported_affected_services(&reported, &known_services);
    mark_replacement_failure(state, project_id, &affected).await;
}

/// Per-call differences for a batch-run; everything else is read from the DB by
/// [`build_batch_run_payload`].
pub(crate) struct BatchRunInputs {
    pub project_id: String,
    pub compose_yaml: String,
    pub service_order: Vec<String>,
    /// If Some, only recreate these services (partial redeploy).
    pub target_services: Option<Vec<String>>,
    pub allow_raw_ports: Option<bool>,
    pub docker_observe: Option<bool>,
    pub host_network: Option<bool>,
    pub is_background: bool,
    pub force_pull: bool,
    pub stage_only: bool,
}

/// Assemble the agent batch-run request shared by start/recreate/deploy/stage.
///
/// Per-service resource overrides and global default limits are read here —
/// tolerantly: a read failure degrades to no overrides / platform defaults.
/// Capability flags are resolved by the caller (fresh lookups for lifecycle
/// paths, persisted values during deploy) and passed in via `inputs`.
pub(crate) async fn build_batch_run_payload(db: &sqlx::SqlitePool, inputs: BatchRunInputs) -> BatchRunRequest {
    let service_resources: HashMap<String, ServiceResources> = sqlx::query_as::<_, (String, Option<i64>, Option<f64>)>(
        "SELECT service_name, memory_limit_mb, cpu_limit FROM project_services WHERE project_id = ?",
    )
    .bind(&inputs.project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(name, memory_limit_mb, cpu_limit)| {
        (memory_limit_mb.is_some() || cpu_limit.is_some())
            .then_some((name, ServiceResources { memory_limit_mb, cpu_limit }))
    })
    .collect();

    let default_memory_limit_mb = read_setting(db, "default_memory_limit_mb", 256).await;
    let default_cpu_limit = read_setting(db, "default_cpu_limit", 0.5).await;

    BatchRunRequest {
        project_id: inputs.project_id,
        compose_yaml: inputs.compose_yaml,
        service_order: inputs.service_order,
        target_services: inputs.target_services,
        allow_raw_ports: inputs.allow_raw_ports,
        docker_observe: inputs.docker_observe,
        host_network: inputs.host_network,
        is_background: inputs.is_background,
        force_pull: inputs.force_pull,
        stage_only: inputs.stage_only,
        service_resources: Some(service_resources),
        default_memory_limit_mb: Some(default_memory_limit_mb),
        default_cpu_limit: Some(default_cpu_limit),
    }
}

async fn read_setting<T: std::str::FromStr>(db: &sqlx::SqlitePool, key: &str, default: T) -> T {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_one(db)
        .await
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{
        cancellation_cleanup_services, proxy_needed_after_stop, select_reported_affected_services,
        should_abort_siblings,
    };
    use std::collections::HashSet;

    #[test]
    fn proxy_remains_until_last_declared_requester_stops() {
        let requesters = HashSet::from(["bridge-observer".into(), "host-observer".into()]);
        let running = HashSet::from(["bridge-observer".into(), "host-observer".into(), "api".into()]);

        assert!(proxy_needed_after_stop(&requesters, &running, Some(&HashSet::from(["bridge-observer".into()]))));
        assert!(!proxy_needed_after_stop(
            &requesters,
            &running,
            Some(&HashSet::from(["bridge-observer".into(), "host-observer".into(),])),
        ));
        assert!(!proxy_needed_after_stop(&requesters, &running, None));
    }

    #[test]
    fn revoked_or_absent_observation_never_retains_proxy() {
        let no_approved_requesters = HashSet::new();
        let running = HashSet::from(["collector".into(), "unrelated".into()]);

        assert!(!proxy_needed_after_stop(
            &no_approved_requesters,
            &running,
            Some(&HashSet::from(["unrelated".into()])),
        ));
        assert!(!proxy_needed_after_stop(&no_approved_requesters, &running, None,));
    }

    #[test]
    fn rollback_metadata_selection_excludes_proxy_and_untouched_rows() {
        let known = HashSet::from(["requested".into(), "implicit-host-observer".into(), "untouched".into()]);
        let reported = vec![
            "requested".into(),
            "implicit-host-observer".into(),
            litebin_common::types::DOCKER_PROXY_SERVICE.into(),
            "not-a-project-service".into(),
        ];

        assert_eq!(
            select_reported_affected_services(&reported, &known),
            HashSet::from(["requested".into(), "implicit-host-observer".into()])
        );
    }

    #[test]
    fn cancellation_aborts_only_when_cleanup_is_required() {
        assert!(!should_abort_siblings(false, false));
        assert!(should_abort_siblings(true, false));
        assert!(should_abort_siblings(false, true));
    }

    #[test]
    fn cancellation_cleanup_excludes_unattempted_preexisting_services() {
        let attempted = HashSet::from(["fresh".into(), "recreated".into()]);
        let started = vec![("fresh".into(), "new-id".into()), ("restarted".into(), "old-id".into())];
        assert_eq!(
            cancellation_cleanup_services(&attempted, &started),
            HashSet::from(["fresh".into(), "recreated".into(), "restarted".into()])
        );
    }
}
