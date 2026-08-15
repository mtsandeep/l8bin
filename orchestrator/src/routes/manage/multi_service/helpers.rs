use std::collections::HashSet;

use axum::http::StatusCode;

use crate::AppState;
use crate::status;

use crate::routes::manage::helpers::read_local_project_env;

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
    let reported = serde_json::from_str::<serde_json::Value>(response_body)
        .ok()
        .and_then(|body| body["affected_services"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|service| service.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
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
