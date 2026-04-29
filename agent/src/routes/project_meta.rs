use std::collections::HashMap;

use axum::{extract::State, http::StatusCode, Json};

use crate::{AgentState, ProjectMetaEntry};

#[derive(serde::Deserialize)]
pub struct ProjectMetaRequest {
    pub projects: HashMap<String, bool>,
    pub allow_raw_ports: Option<HashMap<String, bool>>,
}

/// POST /internal/project-meta — called by orchestrator to push auto_start_enabled + allow_raw_ports flags.
/// Replaces the full project meta map, persists to disk + memory.
pub async fn update_project_meta(
    State(state): State<AgentState>,
    Json(req): Json<ProjectMetaRequest>,
) -> StatusCode {
    tracing::info!(count = req.projects.len(), "received project meta from orchestrator");

    // Start from auto_start_enabled, then overlay allow_raw_ports
    let mut meta: HashMap<String, ProjectMetaEntry> = req.projects.into_iter()
        .map(|(id, auto)| (id, ProjectMetaEntry { auto_start_enabled: auto, allow_raw_ports: false }))
        .collect();

    if let Some(raw_ports) = req.allow_raw_ports {
        for (id, val) in raw_ports {
            meta.entry(id).or_default().allow_raw_ports = val;
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
