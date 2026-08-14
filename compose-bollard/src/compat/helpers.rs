use super::{CompatibilityFinding, FindingDisposition};

pub(super) fn finding(
    path: impl Into<String>,
    service: Option<String>,
    disposition: FindingDisposition,
    message: impl Into<String>,
    capability: Option<&str>,
) -> CompatibilityFinding {
    CompatibilityFinding {
        path: path.into(),
        service,
        disposition,
        message: message.into(),
        capability: capability.map(|s| s.to_string()),
    }
}

pub(super) fn is_docker_sock_source(source: &str) -> bool {
    matches!(normalize_unix_path(source).as_deref(), Some("/var/run/docker.sock" | "/run/docker.sock"))
}

pub(super) fn docker_socket_is_below(source: &str) -> bool {
    let Some(source) = normalize_unix_path(source) else {
        return false;
    };
    ["/var/run/docker.sock", "/run/docker.sock"]
        .iter()
        .any(|socket| source == "/" || socket.starts_with(&format!("{source}/")))
}

fn normalize_unix_path(path: &str) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    let mut components = Vec::new();
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

pub(super) fn volume_source(volume: &str) -> &str {
    volume.split(':').next().unwrap_or(volume).trim()
}

/// A bind source that refers to the repo working tree (e.g. `./scripts/x.sh`,
/// `../config`, `.`) — distinct from named volumes, absolute host paths, and
/// env-interpolated paths (`${VAR}`). LiteBin remaps these under the project
/// directory but does not transfer their contents to the node.
pub(super) fn is_repo_relative_bind(source: &str) -> bool {
    source == "." || source.starts_with("./") || source.starts_with("../")
}

/// Docker container name LiteBin will assign (matches `litebin_common::container_name`).
pub(super) fn managed_container_name(project_id: &str, service: &str) -> String {
    if service == "web" {
        format!("litebin-{project_id}")
    } else {
        format!("litebin-{project_id}.{service}")
    }
}

pub(super) fn managed_network_name(project_id: &str) -> String {
    format!("litebin-{project_id}")
}
