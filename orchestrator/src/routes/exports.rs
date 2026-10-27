//! Export bundle folder management: list, download, delete, and job status.
//!
//! Bundles land in the exports folder (`data/exports`, host-bind mounted in
//! prod) so they can be scp'd off the box as well as downloaded. Filenames
//! are strictly validated — no separators, no `..` — and canonicalized inside
//! the folder before any filesystem touch.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Serialize;
use tokio_util::io::ReaderStream;

use crate::AppState;
use crate::copy_engine;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ExportJob {
    pub id: String,
    pub project_id: String,
    /// `running` | `completed` | `failed`.
    pub status: String,
    pub error: Option<String>,
    /// Bundle file name inside the exports folder (set on completion).
    pub artifact: Option<String>,
    pub size_bytes: Option<u64>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ExportSummary {
    pub file: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// A file name is safe when it is a single path component of allowed chars.
pub(crate) fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        && !name.contains("..")
}

/// Resolve a request file name to a path inside the exports folder, or None
/// when the name is unsafe.
fn export_path(name: &str) -> Option<std::path::PathBuf> {
    if !valid_file_name(name) {
        return None;
    }
    let path = copy_engine::exports_dir().join(name);
    // Belt-and-suspenders: canonical form must stay inside the folder.
    match std::path::Path::new(&path).canonicalize() {
        Ok(p) => p.starts_with(copy_engine::exports_dir().canonicalize().ok()?).then_some(p),
        Err(_) => None,
    }
}

#[utoipa::path(
    get,
    path = "/exports",
    description = "List export bundles in the server's exports folder.",
    responses((status = 200, body = ExportListResponse)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn list_exports(State(state): State<AppState>) -> impl IntoResponse {
    let mut exports: Vec<ExportSummary> = Vec::new();
    let dir = copy_engine::exports_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".l8b.tar.gz") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                exports.push(ExportSummary {
                    file: name,
                    size_bytes: meta.len(),
                    created_at: meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                });
            }
        }
    }
    exports.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let folder_path = state.docker.host_bind_source("/app/data/exports").await;
    (StatusCode::OK, Json(serde_json::json!({ "exports": exports, "folder_path": folder_path })))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ExportListResponse {
    pub exports: Vec<ExportSummary>,
    /// Host-side path of the exports folder (resolved from this container's
    /// bind mounts); null when it can't be detected (bare-metal/dev).
    pub folder_path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/exports/{file}",
    description = "Download an export bundle (streamed gzip tar).",
    params(("file" = String, Path, description = "Bundle file name")),
    responses(
        (status = 200, description = "Bundle bytes", content_type = "application/gzip"),
        (status = 404, description = "Not found"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn download_export(State(_state): State<AppState>, Path(file): Path<String>) -> impl IntoResponse {
    let Some(path) = export_path(&file) else {
        return (StatusCode::NOT_FOUND, "no such export").into_response();
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return (StatusCode::NOT_FOUND, "no such export").into_response();
    };
    let Ok(f) = tokio::fs::File::open(&path).await else {
        return (StatusCode::NOT_FOUND, "no such export").into_response();
    };
    let stream = ReaderStream::with_capacity(f, 1024 * 1024);
    let body = axum::body::Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "application/gzip".parse().unwrap());
    headers.insert(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}\"").parse().unwrap());
    headers.insert(header::CONTENT_LENGTH, meta.len().into());
    (StatusCode::OK, headers, body).into_response()
}

#[utoipa::path(
    delete,
    path = "/exports/{file}",
    description = "Delete an export bundle from the exports folder.",
    params(("file" = String, Path, description = "Bundle file name")),
    responses((status = 204), (status = 404)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn delete_export(State(_state): State<AppState>, Path(file): Path<String>) -> impl IntoResponse {
    match export_path(&file) {
        Some(path) => match std::fs::remove_file(&path) {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/exports/jobs/{id}",
    description = "Poll an export job (started by POST /projects/{id}/export).",
    params(("id" = String, Path, description = "Job ID")),
    responses((status = 200, body = ExportJob), (status = 404)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn export_job_status(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match state.export_jobs.get(&id) {
        Some(job) => (StatusCode::OK, Json(job.clone())).into_response(),
        None => (StatusCode::NOT_FOUND, "no such export job").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::valid_file_name;

    #[test]
    fn rejects_traversal_and_unsafe_names() {
        assert!(valid_file_name("my-app-20260101-000000.l8b.tar.gz"));
        assert!(!valid_file_name("../etc/passwd"));
        assert!(!valid_file_name("a/b"));
        assert!(!valid_file_name(".."));
        assert!(!valid_file_name(""));
        assert!(!valid_file_name(".hidden"));
        assert!(!valid_file_name("weird..name"));
    }
}
