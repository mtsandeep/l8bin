use super::*;

#[test]
fn classify_not_found() {
    let e = bollard::errors::Error::DockerResponseServerError { status_code: 404, message: "No such network".into() };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::NotFound);
}

#[test]
fn classify_conflict() {
    let e = bollard::errors::Error::DockerResponseServerError {
        status_code: 409,
        message: "container already running".into(),
    };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Conflict);
}

#[test]
fn classify_forbidden() {
    let e = bollard::errors::Error::DockerResponseServerError {
        status_code: 403,
        message: "container already connected to network".into(),
    };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Forbidden);
}

#[test]
fn classify_server_error_as_other() {
    let e =
        bollard::errors::Error::DockerResponseServerError { status_code: 500, message: "internal server error".into() };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Other);
}

#[test]
fn classify_io_error_as_connection() {
    let e =
        bollard::errors::Error::IOError { err: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused") };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Connection);
}

#[test]
fn classify_timeout() {
    let e = bollard::errors::Error::RequestTimeoutError;
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Timeout);
}

#[test]
fn classify_anyhow_wrapping_bollard() {
    let inner =
        bollard::errors::Error::DockerResponseServerError { status_code: 404, message: "No such container".into() };
    let anyhow_err: anyhow::Error = inner.into();
    assert_eq!(DockerErrorKind::from_anyhow(&anyhow_err), DockerErrorKind::NotFound);
}

#[test]
fn stopping_an_already_stopped_or_absent_container_is_idempotent() {
    for status_code in [304, 404] {
        let error = bollard::errors::Error::DockerResponseServerError {
            status_code,
            message: "container is not running or does not exist".into(),
        };
        assert!(super::container::is_idempotent_container_stop_error(&error));
    }

    let error =
        bollard::errors::Error::DockerResponseServerError { status_code: 500, message: "daemon failure".into() };
    assert!(!super::container::is_idempotent_container_stop_error(&error));
}

#[test]
fn project_workload_identity_accepts_current_and_legacy_containers() {
    let labels = HashMap::from([("litebin.project_id".into(), "my-app".into())]);
    assert!(super::container::is_project_workload_container("my-app", &["/replacement-name".into()], Some(&labels),));
    assert!(super::container::is_project_workload_container("my-app", &["/litebin-my-app".into()], None,));
    assert!(super::container::is_project_workload_container("my-app", &["/litebin-my-app.worker".into()], None,));
}

#[test]
fn project_workload_identity_rejects_other_projects_and_managed_proxy() {
    let other_labels = HashMap::from([("litebin.project_id".into(), "my-app-2".into())]);
    assert!(!super::container::is_project_workload_container(
        "my-app",
        &["/litebin-my-app.worker".into()],
        Some(&other_labels),
    ));
    assert!(!super::container::is_project_workload_container("my-app", &["/litebin-my-app-2".into()], None,));
    assert!(!super::container::is_project_workload_container(
        "my-app",
        &["/litebin-my-app.litebin-docker-proxy".into()],
        None,
    ));
}

#[test]
fn raw_docker_socket_is_removed_from_workloads_even_when_read_only() {
    let binds = vec!["/var/run/docker.sock:/var/run/docker.sock:ro".to_string(), "litebin_data:/data".to_string()];
    let sanitized = super::container::sanitize_docker_socket_binds(&binds, false);
    assert_eq!(sanitized, vec!["litebin_data:/data"]);
}

#[test]
fn raw_docker_socket_is_retained_only_for_managed_proxy() {
    let binds = vec!["/var/run/docker.sock:/var/run/docker.sock".to_string()];
    assert_eq!(super::container::sanitize_docker_socket_binds(&binds, true), binds);
}

#[test]
fn ancestor_binds_that_expose_docker_socket_are_removed() {
    let binds = vec![
        "/:/host:ro".to_string(),
        "/var:/host-var".to_string(),
        "/var/run/..:/host-var-normalized".to_string(),
        "/safe:/safe".to_string(),
    ];
    let sanitized = super::container::sanitize_docker_socket_binds(&binds, false);
    assert_eq!(sanitized, vec!["/safe:/safe"]);
}

#[test]
fn unrelated_socket_names_are_not_removed() {
    let binds = vec!["/tmp/my-docker.sock:/tmp/docker.sock".to_string(), "/safe:/safe".to_string()];
    let sanitized = super::container::sanitize_docker_socket_binds(&binds, false);
    assert_eq!(sanitized, binds);
}

#[test]
fn managed_docker_host_replaces_compose_value_once() {
    let merged = super::container::merge_service_env(
        vec![
            "DOCKER_HOST=tcp://attacker:2375".into(),
            "KEEP=value".into(),
            "DOCKER_HOST=unix:///var/run/docker.sock".into(),
        ],
        &["DOCKER_HOST=tcp://litebin-docker-proxy:2375".into()],
        true,
    );
    assert_eq!(merged, vec!["KEEP=value", "DOCKER_HOST=tcp://litebin-docker-proxy:2375"]);
}

#[test]
fn full_project_cleanup_selects_workload_proxy_and_private_network() {
    let project_id = "generic-cleanup";
    let prefixes = super::container::project_cleanup_container_prefixes(project_id);
    let workload = crate::types::container_name(project_id, "collector", None);
    let proxy = crate::types::container_name(project_id, crate::types::DOCKER_PROXY_SERVICE, None);

    assert!(prefixes.iter().any(|prefix| workload.starts_with(prefix)));
    assert!(prefixes.iter().any(|prefix| proxy.starts_with(prefix)));
    assert_eq!(
        super::container::project_cleanup_observe_network(project_id),
        crate::types::docker_observe_network_name(project_id, None)
    );
}

#[test]
fn app_project_network_excludes_observation_and_defaults() {
    use super::image::is_app_project_network;

    assert!(is_app_project_network("litebin-myapp"));
    assert!(!is_app_project_network("litebin-myapp-docker-observe"));
    assert!(!is_app_project_network(crate::types::DEFAULT_DOCKER_NETWORK));
    assert!(!is_app_project_network("bridge"));
    assert!(!is_app_project_network("other-net"));
}

#[test]
fn relative_bind_host_path_uses_projects_dir() {
    use super::image::relative_bind_host_path;

    let path = relative_bind_host_path("projects/myapp/data");
    assert_eq!(path, crate::types::projects_dir().join("myapp").join("data"));
}

#[test]
fn host_network_startup_stabilization_only_applies_to_workload_daemons() {
    use super::container::should_stabilize_startup;

    assert!(should_stabilize_startup(true, false, false));
    assert!(!should_stabilize_startup(false, false, false));
    assert!(!should_stabilize_startup(true, true, false));
    assert!(!should_stabilize_startup(true, false, true));
}

#[test]
fn startup_process_decision_only_fails_confirmed_exits() {
    use super::container::{StartupProcessState, startup_process_state};

    assert_eq!(startup_process_state(Some(false), Some(98)), StartupProcessState::Exited(Some(98)));
    assert_eq!(startup_process_state(Some(false), None), StartupProcessState::Exited(None));
    assert_eq!(startup_process_state(Some(true), Some(1)), StartupProcessState::RunningOrUnknown);
    assert_eq!(startup_process_state(None, None), StartupProcessState::RunningOrUnknown);
}

#[test]
fn startup_log_tail_removes_terminal_controls_and_normalizes_returns() {
    let sanitized = super::container::sanitize_startup_log_chunks([
        "\u{1b}[31merror\u{1b}[0m: address already in use\r",
        "\0listen failed\u{7}\n",
    ]);

    assert_eq!(sanitized, "error: address already in use\nlisten failed");
}

#[test]
fn startup_log_tail_is_bounded_and_keeps_the_end() {
    let prefix = "x".repeat(super::container::STARTUP_LOG_MAX_CHARS);
    let sanitized = super::container::sanitize_startup_log_chunks([prefix.as_str(), "actionable ending"]);

    assert_eq!(sanitized.chars().count(), super::container::STARTUP_LOG_MAX_CHARS);
    assert!(sanitized.ends_with("actionable ending"));
}

#[test]
fn classify_anyhow_without_bollard_as_other() {
    let anyhow_err: anyhow::Error = anyhow::anyhow!("some unrelated error");
    assert_eq!(DockerErrorKind::from_anyhow(&anyhow_err), DockerErrorKind::Other);
}
