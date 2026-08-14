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

pub(super) use crate::naming::{bind_source_exposes_docker_socket, container_name, is_docker_socket_source};

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
    container_name(project_id, service, None)
}

pub(super) fn managed_network_name(project_id: &str) -> String {
    crate::naming::project_network_name(project_id, None)
}
