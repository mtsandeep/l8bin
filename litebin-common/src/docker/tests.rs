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
fn classify_anyhow_without_bollard_as_other() {
    let anyhow_err: anyhow::Error = anyhow::anyhow!("some unrelated error");
    assert_eq!(DockerErrorKind::from_anyhow(&anyhow_err), DockerErrorKind::Other);
}
