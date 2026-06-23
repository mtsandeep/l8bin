use serde::{Deserialize, Serialize};

use crate::types::DeployType;

/// A group of foreign (non-LiteBin-managed) containers discovered during a scan.
/// Either a Docker Compose project (all its services) or a single standalone container.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScanGroup {
    /// Docker Compose project name, or container name for standalone containers.
    pub group_key: String,
    /// Sanitised, DNS-safe project ID suggested for import.
    pub suggested_project_id: String,
    pub deploy_type: DeployType,
    /// Value of `com.docker.compose.project.working_dir` label, if present.
    pub compose_working_dir: Option<String>,
    /// True if a compose.yaml / docker-compose.yml was found at `compose_working_dir`.
    pub compose_file_found: bool,
    /// True if a `.env` file was found at `compose_working_dir`.
    pub env_file_found: bool,
    pub containers: Vec<ScanContainer>,
}

/// A single container within a `ScanGroup`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScanContainer {
    pub container_id: String,
    pub original_name: String,
    /// Service name from `com.docker.compose.service` label, or "web" for standalone.
    pub service_name: String,
    pub image: String,
    /// Docker state: "running", "exited", "paused", etc.
    pub state: String,
    pub ports: Vec<ScannedPort>,
    pub volumes: Vec<ScannedVolume>,
    /// True for the container most likely intended as the public-facing service
    /// (has external port bindings; lowest internal port wins if multiple qualify).
    pub suggested_public: bool,
    /// True if the image was built locally (no registry host, no namespace).
    /// Such projects can be imported but cannot be re-pulled by LiteBin on redeploy.
    pub image_is_local: bool,
}

/// A port binding on a scanned container.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScannedPort {
    /// Container-side port.
    pub internal: u16,
    /// Host-side mapped port (None if not published).
    pub external: Option<u16>,
    pub protocol: String,
}

/// A volume mount on a scanned container.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScannedVolume {
    /// Resolved host-side source path (bind mount) or Docker volume name.
    pub source: String,
    /// Container-side destination path.
    pub destination: String,
    /// "bind" or "volume".
    pub volume_type: String,
}

/// Returns true if an image name looks like a locally-built image
/// (no registry host, no Docker Hub namespace, not a sha256 digest).
/// Examples that return true:  "composehero-core-agent", "myapp"
/// Examples that return false: "nginx", "nginx:latest", "ghcr.io/foo/bar", "sha256:abc"
///
/// Note: plain official images like "nginx" also have no slash but ARE pullable from Docker Hub.
/// We use the presence of a registry label (`com.docker.compose.image` being absent but
/// the container having a `build` compose label) as the primary signal, falling back to
/// whether the image name matches the compose project-service naming pattern.
pub fn is_local_image(image: &str) -> bool {
    // sha256 digests: locally built but unnamed — definitely not pullable
    if image.starts_with("sha256:") {
        return true;
    }
    // Strip optional tag
    let name_part = image.split(':').next().unwrap_or(image);
    // If there's a slash, it has a namespace/registry → pullable
    if name_part.contains('/') {
        return false;
    }
    // If the first component contains a dot or a port-style colon, it's a registry host
    // (already handled by slash check above for most cases, but belt-and-suspenders)
    // Official single-word images (nginx, redis, postgres…) are pullable from Docker Hub.
    // Locally-built images from docker-compose default to "<project>-<service>" pattern
    // which always contains a hyphen and no dots. We can't be 100% certain without
    // querying the Docker daemon, so we just flag the sha256 case definitively and
    // leave everything else as non-local (safer default — user sees no false warnings
    // for official images).
    false
}

/// The combined result from `GET /scan?node_id=all`.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScanResult {
    pub local: Vec<ScanGroup>,
    /// Map of node_id → groups found on that agent node.
    pub nodes: std::collections::HashMap<String, Vec<ScanGroup>>,
}

/// Convert a raw string (container name, compose project name) into a DNS-safe project ID.
pub fn sanitize_project_id(name: &str) -> String {
    let lower = name.to_lowercase();
    // Replace anything that isn't [a-z0-9] with '-'
    let mut result = String::with_capacity(lower.len());
    let mut last_dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            result.push(c);
            last_dash = false;
        } else {
            if !last_dash {
                result.push('-');
            }
            last_dash = true;
        }
    }
    // Strip leading/trailing dashes and truncate to 63 chars
    let trimmed = result.trim_matches('-');
    trimmed.chars().take(63).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_project_id() {
        assert_eq!(sanitize_project_id("MyApp_v2"), "myapp-v2");
        assert_eq!(sanitize_project_id("my--app"), "my-app");
        assert_eq!(sanitize_project_id("--leading"), "leading");
        assert_eq!(sanitize_project_id("trailing--"), "trailing");
        assert_eq!(sanitize_project_id("hello world"), "hello-world");
    }
}
