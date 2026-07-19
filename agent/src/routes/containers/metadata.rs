use litebin_common::types::VolumeMount;
use serde::{Deserialize, Serialize};

use super::env::projects_dir;

// ── Project Metadata ─────────────────────────────────────────────────────────

/// Metadata needed to recreate a container without asking the orchestrator.
#[derive(Serialize, Deserialize, Clone)]
pub struct ProjectMetadata {
    pub image: String,
    pub internal_port: Option<i64>,
    pub cmd: Option<String>,
    pub memory_limit_mb: Option<i64>,
    pub cpu_limit: Option<f64>,
    pub volumes: Option<Vec<VolumeMount>>,
}

/// Path to the metadata file for a project.
pub fn metadata_path(project_id: &str) -> std::path::PathBuf {
    projects_dir().join(project_id).join("metadata.json")
}

/// Write project metadata to disk after successful container creation.
pub fn write_project_metadata(
    project_id: &str,
    image: &str,
    internal_port: Option<i64>,
    cmd: Option<&str>,
    memory_limit_mb: Option<i64>,
    cpu_limit: Option<f64>,
    volumes: Option<Vec<VolumeMount>>,
) {
    let meta = ProjectMetadata {
        image: image.to_string(),
        internal_port,
        cmd: cmd.map(|s| s.to_string()),
        memory_limit_mb,
        cpu_limit,
        volumes,
    };
    let path = metadata_path(project_id);
    let meta_json = match serde_json::to_string_pretty(&meta) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(project = project_id, error = %e, "failed to serialize metadata.json");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, &meta_json) {
        tracing::warn!(project = project_id, error = %e, "failed to write metadata.json");
    } else {
        tracing::info!(project = project_id, "wrote metadata.json");
    }
}

/// Read project metadata from disk. Returns None if file doesn't exist or is invalid.
pub fn read_project_metadata(project_id: &str) -> Option<ProjectMetadata> {
    let path = metadata_path(project_id);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}
