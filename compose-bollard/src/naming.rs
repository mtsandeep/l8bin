//! Container/network naming and Docker-socket path helpers.
//!
//! Owned by compose-bollard (the bottom crate) so both compose-bollard and
//! litebin-common share one implementation; litebin-common re-exports these.

/// Normalize a Unix path: resolve `.`/`..` segments and collapse slashes.
/// Returns `None` for relative paths.
pub fn normalize_unix_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut components: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value),
        }
    }
    Some(format!("/{}", components.join("/")))
}

/// True when the bind source is exactly a Docker daemon socket path.
pub fn is_docker_socket_source(source: &str) -> bool {
    matches!(normalize_unix_path(source).as_deref(), Some("/var/run/docker.sock" | "/run/docker.sock"))
}

/// True when a bind source mounts the Docker daemon socket itself or any
/// ancestor directory of it (which exposes the socket).
pub fn bind_source_exposes_docker_socket(source: &str) -> bool {
    let Some(source) = normalize_unix_path(source) else {
        return false;
    };
    ["/var/run/docker.sock", "/run/docker.sock"]
        .iter()
        .any(|socket| source == *socket || source == "/" || socket.starts_with(&format!("{source}/")))
}

/// Deterministic workload container name:
/// - primary web service: `litebin-{project_id}`
/// - other services: `litebin-{project_id}.{service}`
/// - replacement instance: `litebin-{project_id}.{service}.{instance_id}`
pub fn container_name(project_id: &str, service_name: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("litebin-{}.{}.{}", project_id, service_name, id),
        None => {
            if service_name == "web" {
                format!("litebin-{}", project_id)
            } else {
                format!("litebin-{}.{}", project_id, service_name)
            }
        }
    }
}

/// Build the per-project Docker network name.
/// - Primary: `litebin-{project_id}`
/// - With instance: `litebin-{project_id}-{instance_id}`
pub fn project_network_name(project_id: &str, instance_id: Option<&str>) -> String {
    match instance_id {
        Some(id) => format!("litebin-{}-{}", project_id, id),
        None => format!("litebin-{}", project_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_and_ancestors_are_detected() {
        assert!(is_docker_socket_source("/var/run/docker.sock"));
        assert!(is_docker_socket_source("/run/docker.sock/../docker.sock"));
        assert!(!is_docker_socket_source("/tmp/docker.sock"));
        assert!(bind_source_exposes_docker_socket("/var"));
        assert!(bind_source_exposes_docker_socket("/"));
        assert!(!bind_source_exposes_docker_socket("/tmp"));
        assert!(!bind_source_exposes_docker_socket("relative/path"));
    }

    #[test]
    fn naming_conventions() {
        assert_eq!(container_name("app", "web", None), "litebin-app");
        assert_eq!(container_name("app", "worker", None), "litebin-app.worker");
        assert_eq!(container_name("app", "worker", Some("x1")), "litebin-app.worker.x1");
        assert_eq!(project_network_name("app", None), "litebin-app");
        assert_eq!(project_network_name("app", Some("x1")), "litebin-app-x1");
    }
}
