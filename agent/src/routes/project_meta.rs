use std::collections::HashMap;

use axum::{extract::State, http::StatusCode, Json};

use crate::AgentState;

#[derive(serde::Deserialize)]
pub struct ProjectMetaRequest {
    pub projects: HashMap<String, bool>,
}

/// POST /internal/project-meta — called by orchestrator to push auto_start_enabled flags.
/// Replaces the full project meta map, persists to disk + memory.
pub async fn update_project_meta(
    State(state): State<AgentState>,
    Json(req): Json<ProjectMetaRequest>,
) -> StatusCode {
    tracing::info!(count = req.projects.len(), "received project meta from orchestrator");

    // Update in-memory state
    {
        let mut guard = state.project_meta.write().unwrap();
        *guard = req.projects;
    }

    // Persist to disk
    crate::save_project_meta_to_file(&state.project_meta.read().unwrap());

    StatusCode::OK
}
