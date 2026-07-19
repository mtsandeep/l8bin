use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use litebin_common::types::COMPOSE_FILE_NAMES;
use serde::{Deserialize, Serialize};

use crate::AgentState;

use super::env::projects_dir;

// ── Import types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ContainerImportSpec {
    pub container_id: String,
    pub new_name: String,
}

#[derive(Deserialize)]
pub struct ImportRequest {
    pub project_id: String,
    pub network_name: String,
    pub containers: Vec<ContainerImportSpec>,
    pub compose_yaml: Option<String>,
    pub env_content: Option<String>,
}

#[derive(Serialize)]
pub struct ContainerImportResult {
    pub container_id: String,
    pub new_name: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ImportResponse {
    pub results: Vec<ContainerImportResult>,
    pub errors: Vec<String>,
}

// ── Scan & Import Handlers ───────────────────────────────────────────────────

/// GET /containers/scan
/// Returns foreign (non-LiteBin-managed) container groups detected on this node.
pub async fn scan_containers(State(state): State<AgentState>) -> impl IntoResponse {
    match state.docker.scan_foreign_containers().await {
        Ok(groups) => (StatusCode::OK, Json(groups)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "scan: failed to scan foreign containers");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// POST /containers/import
/// Performs the Docker-side structural import for a single project group:
/// renames containers, creates litebin network, connects all containers,
/// and writes compose.yaml + .env to disk.
pub async fn import_containers(State(state): State<AgentState>, Json(req): Json<ImportRequest>) -> impl IntoResponse {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    for spec in &req.containers {
        let inspect = match state.docker.inspect_container(&spec.container_id).await {
            Ok(inspect) => inspect,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("cannot inspect container before import: {e}")
                    })),
                )
                    .into_response();
            }
        };
        let exposes_socket = inspect.mounts.as_ref().is_some_and(|mounts| {
            mounts.iter().any(|mount| {
                mount.source.as_deref().is_some_and(litebin_common::docker::bind_source_exposes_docker_socket)
            })
        });
        if exposes_socket {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "cannot import a container whose host mounts expose the Docker daemon socket"
                })),
            )
                .into_response();
        }
    }

    // 1. Rename each container (live — container keeps running)
    for spec in &req.containers {
        match state.docker.rename_container(&spec.container_id, &spec.new_name).await {
            Ok(()) => {
                results.push(ContainerImportResult {
                    container_id: spec.container_id.clone(),
                    new_name: spec.new_name.clone(),
                    ok: true,
                    error: None,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, container_id = %spec.container_id, "import: rename failed");
                results.push(ContainerImportResult {
                    container_id: spec.container_id.clone(),
                    new_name: spec.new_name.clone(),
                    ok: false,
                    error: Some(e.to_string()),
                });
                errors.push(format!("rename {} -> {}: {}", spec.container_id, spec.new_name, e));
            }
        }
    }

    // 2. Create per-project litebin network (idempotent)
    if let Err(e) = state.docker.ensure_project_network(&req.project_id, None).await {
        tracing::error!(error = %e, project_id = %req.project_id, "import: failed to create project network");
        errors.push(format!("create network {}: {}", req.network_name, e));
    }

    // 3. Connect each container to the new litebin network
    for spec in &req.containers {
        if let Err(e) = state.docker.connect_container_to_network(&spec.container_id, &req.network_name).await {
            tracing::warn!(error = %e, container = %spec.new_name, network = %req.network_name, "import: connect to network failed");
            errors.push(format!("connect {} to {}: {}", spec.new_name, req.network_name, e));
        }
    }

    // 4. Connect the agent itself to the new network so it can proxy
    let agent_id = std::env::var("AGENT_CONTAINER_NAME").unwrap_or_else(|_| "litebin-agent".into());
    if let Err(e) = state.docker.connect_container_to_network(&agent_id, &req.network_name).await {
        tracing::debug!(error = %e, "import: agent connect to network skipped (likely already connected)");
    }

    // 5. Write compose.yaml + .env to projects/{project_id}/
    let project_dir = projects_dir().join(&req.project_id);
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        errors.push(format!("create project dir: {e}"));
    } else {
        if let Some(ref compose_content) = req.compose_yaml {
            let compose_path = project_dir.join("compose.yaml");
            if let Err(e) = std::fs::write(&compose_path, compose_content) {
                errors.push(format!("write compose.yaml: {e}"));
            } else {
                tracing::info!(path = ?compose_path, "import: wrote compose.yaml");
            }
        }
        if let Some(ref env_content) = req.env_content {
            let env_path = project_dir.join(".env");
            if let Err(e) = std::fs::write(&env_path, env_content) {
                errors.push(format!("write .env: {e}"));
            } else {
                tracing::info!(path = ?env_path, "import: wrote .env");
            }
        }
    }

    (StatusCode::OK, Json(ImportResponse { results, errors })).into_response()
}

/// GET /containers/compose-file?dir=<path>
/// Reads compose.yaml and .env from the given working directory on the host.
/// Used by the orchestrator during import (it can't read host paths from inside Docker).
pub async fn get_compose_file(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let dir = match params.get("dir") {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "missing `dir` query param" })))
                .into_response();
        }
    };

    // Validate: must be an absolute path pointing to an existing directory
    if !dir.is_absolute() || !dir.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "dir must be an absolute path to an existing directory" })),
        )
            .into_response();
    }

    // Canonicalize to resolve symlinks and .. components
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "directory does not exist" })))
                .into_response();
        }
    };

    let compose_yaml: Option<String> =
        COMPOSE_FILE_NAMES.iter().find_map(|name| std::fs::read_to_string(dir.join(name)).ok());

    let env_content: Option<String> = std::fs::read_to_string(dir.join(".env")).ok();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "compose_yaml": compose_yaml,
            "env_content": env_content,
        })),
    )
        .into_response()
}
