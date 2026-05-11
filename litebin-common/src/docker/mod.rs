use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use serde::{Serialize, Deserialize};

mod container;
mod image;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use container::*;
#[allow(unused_imports)]
pub use image::*;

/// Classification of a bollard::Error for control-flow decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerErrorKind {
    /// HTTP 404 — resource does not exist (network, volume, container, image).
    NotFound,
    /// HTTP 409 — conflict (e.g., container name already in use).
    Conflict,
    /// HTTP 403 — forbidden (e.g., container already connected to network).
    Forbidden,
    /// HTTP 400 — bad request.
    BadRequest,
    /// Timeout / HTTP 408 or 504.
    Timeout,
    /// Connection/IO errors — Docker daemon unreachable.
    Connection,
    /// Any other error.
    Other,
}

impl DockerErrorKind {
    /// Classify a bollard::Error by matching on its variant structure
    pub fn from_bollard_error(e: &bollard::errors::Error) -> Self {
        match e {
            bollard::errors::Error::DockerResponseServerError { status_code, .. } => match *status_code {
                404 => Self::NotFound,
                409 => Self::Conflict,
                403 => Self::Forbidden,
                400 => Self::BadRequest,
                408 | 504 => Self::Timeout,
                _ => Self::Other,
            },
            bollard::errors::Error::IOError { .. }
            | bollard::errors::Error::HyperResponseError { .. } => Self::Connection,
            bollard::errors::Error::RequestTimeoutError => Self::Timeout,
            _ => Self::Other,
        }
    }

    /// Classify an anyhow::Error by downcasting to bollard::Error.
    /// Falls back to `Other` if the error chain doesn't contain a bollard::Error.
    pub fn from_anyhow(err: &anyhow::Error) -> Self {
        err.downcast_ref::<bollard::errors::Error>()
            .map(Self::from_bollard_error)
            .unwrap_or(Self::Other)
    }
}

/// Cached CPU stats sample for computing deltas between readings.
#[derive(Clone)]
pub(crate) struct CpuSample {
    pub(crate) total_usage: u64,
    pub(crate) system_cpu_usage: u64,
}

pub struct DockerManager {
    pub(crate) docker: Docker,
    pub(crate) network: String,
    pub(crate) memory_limit: Arc<AtomicI64>,
    pub(crate) cpu_limit: Arc<AtomicU64>, // f64 stored as bits
    /// Host-side path for the projects directory (detected via self-inspection).
    /// Used to translate `projects/...` bind mount paths to host paths.
    pub(crate) host_projects_dir: Option<String>,
    /// Cached CPU stat samples per container_id for delta computation.
    pub(crate) cpu_samples: Arc<Mutex<HashMap<String, CpuSample>>>,
}

impl Clone for DockerManager {
    fn clone(&self) -> Self {
        Self {
            docker: self.docker.clone(),
            network: self.network.clone(),
            memory_limit: Arc::clone(&self.memory_limit),
            cpu_limit: Arc::clone(&self.cpu_limit),
            host_projects_dir: self.host_projects_dir.clone(),
            cpu_samples: Arc::clone(&self.cpu_samples),
        }
    }
}

impl DockerManager {
    pub fn new(network: String, memory_limit: i64, cpu_limit: f64) -> anyhow::Result<Self> {
        let docker = Docker::connect_with_socket_defaults()?;
        Ok(Self {
            docker,
            network,
            memory_limit: Arc::new(AtomicI64::new(memory_limit)),
            cpu_limit: Arc::new(AtomicU64::new(cpu_limit.to_bits())),
            host_projects_dir: None,
            cpu_samples: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Create a DockerManager without connecting to the Docker socket — for use in tests only.
    pub fn new_for_tests() -> Self {
        let docker = Docker::connect_with_http(
            "http://127.0.0.1:1",
            4,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("http docker client");
        Self {
            docker,
            network: "test".to_string(),
            memory_limit: Arc::new(AtomicI64::new(0)),
            cpu_limit: Arc::new(AtomicU64::new(0.0_f64.to_bits())),
            host_projects_dir: None,
            cpu_samples: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Detect the host-side path for the projects directory by inspecting this
    /// container's own mount information via the Docker API. This allows bind
    /// mount sources (which Docker resolves on the host) to use the correct
    /// host path instead of the relative `projects/...` path.
    pub async fn detect_host_projects_dir(&mut self) {
        let hostname = match std::fs::read_to_string("/etc/hostname") {
            Ok(h) => h.trim().to_string(),
            Err(_) => return,
        };

        let inspect = match self.docker.inspect_container(&hostname, None).await {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!("could not inspect own container for host path detection: {e}");
                return;
            }
        };

        if let Some(mounts) = inspect.mounts {
            for mount in mounts {
                if mount.destination.as_deref() == Some("/app/projects") {
                    if let Some(source) = mount.source {
                        tracing::info!(host_projects_dir = %source, "detected host projects directory");
                        self.host_projects_dir = Some(source);
                    }
                    return;
                }
            }
        }
        tracing::debug!("no /app/projects mount found on this container");
    }

    /// Update the default memory and CPU limits used as fallbacks
    /// when per-service limits are not specified.
    pub fn update_defaults(&self, memory_limit: i64, cpu_limit: f64) {
        tracing::info!(memory_bytes = memory_limit, cpu = cpu_limit, "updating DockerManager defaults");
        self.memory_limit.store(memory_limit, Ordering::Relaxed);
        self.cpu_limit.store(cpu_limit.to_bits(), Ordering::Relaxed);
    }

    /// Translate `projects/...` paths in bind specs to host-side paths,
    /// so Docker resolves them correctly on the host.
    pub(crate) fn translate_bind_paths(&self, binds: &mut [String]) {
        if let Some(ref host_dir) = self.host_projects_dir {
            for bind in binds.iter_mut() {
                if let Some(colon_pos) = bind.find(':') {
                    let source = &bind[..colon_pos];
                    if source.starts_with("projects/") {
                        let new_source = format!("{}/{}", host_dir, &source["projects/".len()..]);
                        *bind = format!("{}{}", new_source, &bind[colon_pos..]);
                    }
                }
            }
        }
    }
}

/// Info about a running litebin container, returned by `list_running_litebin_containers`.
#[derive(Debug, Clone, Serialize)]
pub struct RunningContainer {
    pub project_id: String,
    pub service_name: String,
    pub instance_id: Option<String>,
    pub container_name: String,
    pub internal_port: u16,
    pub mapped_port: u16,
}

#[derive(Debug, Serialize)]
pub struct ContainerStats {
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiskUsage {
    pub size_rw: u64,              // Writable layer size (bytes written by container)
    pub size_root_fs: u64,         // Total image + writable layer
    pub cpu_limit: Option<f64>,    // CPU limit from HostConfig.NanoCpus (e.g. 1.5 = 1.5 cores)
}

pub fn is_port_ready(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}
