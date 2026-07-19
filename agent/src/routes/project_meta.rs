use std::collections::HashMap;

use axum::{Json, extract::State, http::StatusCode};

use crate::{AgentState, ProjectMetaEntry};

#[derive(serde::Deserialize)]
pub struct ProjectMetaRequest {
    pub projects: HashMap<String, bool>,
    pub background_projects: Option<HashMap<String, bool>>,
    pub allow_raw_ports: Option<HashMap<String, bool>>,
    pub docker_observe: Option<HashMap<String, bool>>,
    pub host_network: Option<HashMap<String, bool>>,
    /// Global default memory limit (MB) from orchestrator settings.
    pub default_memory_limit_mb: Option<i64>,
    /// Global default CPU limit from orchestrator settings.
    pub default_cpu_limit: Option<f64>,
}

/// POST /internal/project-meta — called by orchestrator to push lifecycle and capability flags.
/// Replaces the full project meta map, persists to disk + memory.
pub async fn update_project_meta(State(state): State<AgentState>, Json(req): Json<ProjectMetaRequest>) -> StatusCode {
    tracing::info!(count = req.projects.len(), "received project meta from orchestrator");

    // Start from auto_start_enabled, then overlay workload and capability flags.
    let mut meta: HashMap<String, ProjectMetaEntry> = req
        .projects
        .into_iter()
        .map(|(id, auto)| {
            (
                id,
                ProjectMetaEntry {
                    auto_start_enabled: auto,
                    is_background: false,
                    allow_raw_ports: false,
                    docker_observe: false,
                    host_network: false,
                    default_memory_limit_mb: req.default_memory_limit_mb,
                    default_cpu_limit: req.default_cpu_limit,
                },
            )
        })
        .collect();

    if let Some(background_projects) = req.background_projects {
        for (id, val) in background_projects {
            meta.entry(id).or_default().is_background = val;
        }
    }

    if let Some(raw_ports) = req.allow_raw_ports {
        for (id, val) in raw_ports {
            meta.entry(id).or_default().allow_raw_ports = val;
        }
    }

    if let Some(docker_observe) = req.docker_observe {
        for (id, val) in docker_observe {
            meta.entry(id).or_default().docker_observe = val;
        }
    }

    if let Some(host_network) = req.host_network {
        for (id, val) in host_network {
            meta.entry(id).or_default().host_network = val;
        }
    }

    // Update in-memory state + persist
    {
        let mut guard = state.project_meta.write().unwrap();
        *guard = meta;
    }

    // Persist to disk
    crate::save_project_meta_to_file(&state.project_meta.read().unwrap());

    StatusCode::OK
}
