use super::*;

#[test]
fn classify_not_found() {
    let e = bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        message: "No such network".into(),
    };
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
    let e = bollard::errors::Error::DockerResponseServerError {
        status_code: 500,
        message: "internal server error".into(),
    };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Other);
}

#[test]
fn classify_io_error_as_connection() {
    let e = bollard::errors::Error::IOError {
        err: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
    };
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Connection);
}

#[test]
fn classify_timeout() {
    let e = bollard::errors::Error::RequestTimeoutError;
    assert_eq!(DockerErrorKind::from_bollard_error(&e), DockerErrorKind::Timeout);
}

#[test]
fn classify_anyhow_wrapping_bollard() {
    let inner = bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        message: "No such container".into(),
    };
    let anyhow_err: anyhow::Error = inner.into();
    assert_eq!(DockerErrorKind::from_anyhow(&anyhow_err), DockerErrorKind::NotFound);
}

#[test]
fn raw_docker_socket_is_removed_from_workloads_even_when_read_only() {
    let binds = vec![
        "/var/run/docker.sock:/var/run/docker.sock:ro".to_string(),
        "litebin_data:/data".to_string(),
    ];
    let sanitized = super::container::sanitize_docker_socket_binds(&binds, false);
    assert_eq!(sanitized, vec!["litebin_data:/data"]);
}

#[test]
fn raw_docker_socket_is_retained_only_for_managed_proxy() {
    let binds = vec!["/var/run/docker.sock:/var/run/docker.sock".to_string()];
    assert_eq!(
        super::container::sanitize_docker_socket_binds(&binds, true),
        binds
    );
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
    let binds = vec![
        "/tmp/my-docker.sock:/tmp/docker.sock".to_string(),
        "/safe:/safe".to_string(),
    ];
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
    assert_eq!(
        merged,
        vec![
            "KEEP=value",
            "DOCKER_HOST=tcp://litebin-docker-proxy:2375"
        ]
    );
}

#[test]
fn classify_anyhow_without_bollard_as_other() {
    let anyhow_err: anyhow::Error = anyhow::anyhow!("some unrelated error");
    assert_eq!(DockerErrorKind::from_anyhow(&anyhow_err), DockerErrorKind::Other);
}
