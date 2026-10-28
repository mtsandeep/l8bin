//! Project export — write a self-contained `.l8b.tar.gz` bundle (manifest +
//! project dir + volumes + uploaded images) into the exports folder and make
//! it downloadable. The source stops briefly for consistent volume tars and
//! restarts afterwards. Doubles as a full (cold) project backup.

use std::collections::VecDeque;
use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::copy_engine::{self, NodeRef, bundle, manifest};
use crate::routes::exports::ExportJob;
use crate::status::{self, ProjectUpdateFields};
use litebin_common::docker::TransferKind;
use litebin_common::types::{Project, ProjectStatus};

use super::stop_start::start_project;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ExportRequest {
    /// Include `.env` secrets in the bundle (default true).
    #[serde(default = "default_true")]
    pub include_env: bool,
    /// Include volume data tars in the bundle (default true).
    #[serde(default = "default_true")]
    pub include_volumes: bool,
}

fn default_true() -> bool {
    true
}

#[utoipa::path(
    post,
    path = "/projects/{id}/export",
    request_body = ExportRequest,
    description = "Export a project as a downloadable/scp-able bundle: stop briefly for a consistent snapshot, write manifest + project files + volumes + uploaded images to the exports folder, restart. The bundle imports on any LiteBin master (POST /imports) and doubles as a full project backup.",
    params(("id" = String, Path, description = "Project ID")),
    responses(
        (status = 202, description = "Export started in background", body = serde_json::Value),
        (status = 404, description = "Project not found"),
        (status = 409, description = "Project is mid-deploy/migrate/export"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn export_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<ExportRequest>,
) -> impl IntoResponse {
    let project = match sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return err(StatusCode::NOT_FOUND, format!("project '{project_id}' not found")),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
    };

    if matches!(
        project.status,
        ProjectStatus::Deploying
            | ProjectStatus::Migrating
            | ProjectStatus::Exporting
            | ProjectStatus::Stopping
            | ProjectStatus::Importing
    ) {
        return err(StatusCode::CONFLICT, format!("project is {} — wait for it to settle", project.status));
    }

    let was_running = matches!(project.status, ProjectStatus::Running | ProjectStatus::Degraded);

    let anon_mounts = super::anonymous_mount_count(&state.db, &project.id).await;
    let mut warnings = Vec::new();
    if anon_mounts > 0 {
        warnings.push(super::anonymous_volume_warning(anon_mounts));
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    state.export_jobs.insert(
        job_id.clone(),
        ExportJob {
            id: job_id.clone(),
            project_id: project.id.clone(),
            status: "running".into(),
            error: None,
            artifact: None,
            size_bytes: None,
            created_at: now,
            finished_at: None,
        },
    );

    if let Err(e) =
        status::transition(&state.db, &project.id, ProjectStatus::Exporting, &ProjectUpdateFields::default(), None)
            .await
    {
        state.export_jobs.remove(&job_id);
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}"));
    }

    let state_bg = state.clone();
    let include_env = payload.include_env;
    let include_volumes = payload.include_volumes;
    let pid = project.id.clone();
    let job_id_bg = job_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_export(&state_bg, &pid, was_running, include_env, include_volumes, &job_id_bg).await {
            tracing::error!(project = %pid, error = %e, "export failed");
            crate::routes::deploy::logs::push_deploy_log(&state_bg, &pid, &format!("export failed: {e}"));
            if let Some(mut job) = state_bg.export_jobs.get_mut(&job_id_bg) {
                job.status = "failed".into();
                job.error = Some(e.to_string());
                job.finished_at = Some(chrono::Utc::now().timestamp());
            }
            // The source was stopped for the snapshot — bring it back if it
            // was running, otherwise settle on stopped.
            if was_running {
                let _ = start_project(axum::extract::State(state_bg.clone()), axum::extract::Path(pid.clone())).await;
            } else {
                let _ = status::transition(
                    &state_bg.db,
                    &pid,
                    ProjectStatus::Stopped,
                    &ProjectUpdateFields::default(),
                    None,
                )
                .await;
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "job_id": job_id,
            "project_id": project.id,
            "status": "exporting",
            "warnings": warnings,
        })),
    )
        .into_response()
}

fn err(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn run_export(
    state: &AppState,
    project_id: &str,
    was_running: bool,
    include_env: bool,
    include_volumes: bool,
    job_id: &str,
) -> anyhow::Result<()> {
    let logs = crate::routes::deploy::logs::push_deploy_log;
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(&state.db)
        .await?;
    let source = match project.node_id.as_deref() {
        Some(n) if n != "local" => NodeRef::Agent(n.to_string()),
        _ => NodeRef::Local,
    };
    let tmp = copy_engine::spool_dir(state);
    tokio::fs::create_dir_all(&tmp).await?;
    tokio::fs::create_dir_all(copy_engine::exports_dir()).await?;

    // 1. Stop for a consistent snapshot.
    logs(state, project_id, "stopping for export snapshot");
    copy_engine::stop_on_source(state, project_id, &source).await?;

    // 2. Manifest.
    let export_source = manifest::ExportSource {
        master_version: env!("CARGO_PKG_VERSION").to_string(),
        node_id: source.node_id().to_string(),
        architecture: source.architecture(state).await,
    };
    let m = manifest::build_from_db(&state.db, &project, export_source, include_env, include_volumes).await?;
    let manifest_json = serde_json::to_vec(&m)?;

    // 3. Bundle: manifest + project dir + volumes + uploaded images.
    let out = copy_engine::exports_dir().join(format!(
        "{}-{}.l8b.tar.gz",
        project.id,
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));

    let mut kinds: Vec<(String, TransferKind)> =
        vec![(bundle::PROJECT_ENTRY.to_string(), TransferKind::ProjectDir(project_id.to_string()))];
    if include_volumes {
        for volume in super::volume_list(&state.db, &project).await {
            kinds.push((format!("volumes/{volume}.tar"), TransferKind::Volume(volume)));
        }
    }
    for image in copy_engine::project_image_refs(&state.db, &project).await {
        if image.starts_with("sha256:") {
            kinds.push((manifest::image_entry_name(&image), TransferKind::Image(image)));
        }
    }
    let total = kinds.len();
    logs(state, project_id, &format!("writing bundle ({total} payloads)"));

    let queue = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::from(kinds)));
    let state_for_closure = state.clone();
    let source_for_closure = source.clone();
    let project_id_for_filter = project_id.to_string();
    let include_env_filter = include_env;
    let next = move || {
        let queue = queue.clone();
        let state_for_closure = state_for_closure.clone();
        let source_for_closure = source_for_closure.clone();
        let tmp = tmp.clone();
        let project_id_for_filter = project_id_for_filter.clone();
        async move {
            let next_item = queue.lock().map(|mut q| q.pop_front()).unwrap_or(None);
            match next_item {
                Some((name, kind)) => {
                    let stream = source_for_closure.transfer_out(&state_for_closure, &kind).await?;
                    // Strip .env* at write time when excluded so secrets never
                    // touch disk inside the bundle.
                    let stream = if matches!(kind, TransferKind::ProjectDir(_)) && !include_env_filter {
                        copy_engine::reroot::reroot(stream, &project_id_for_filter, &project_id_for_filter, false, &tmp)
                            .await?
                    } else {
                        stream
                    };
                    let spool = spool_stream(stream, &tmp, "export").await?;
                    Ok::<_, anyhow::Error>(Some((name, spool)))
                }
                None => Ok(None),
            }
        }
    };
    bundle::write_bundle(&out, manifest_json, next).await?;

    // 4. Restart the source if it was running.
    if was_running {
        logs(state, project_id, "restarting source");
        let _ = start_project(axum::extract::State(state.clone()), axum::extract::Path(project_id.to_string()))
            .await
            .map_err(|(code, message)| anyhow::anyhow!("restart after export failed ({code}): {message}"))?;
    } else {
        status::transition(&state.db, project_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None)
            .await?;
    }
    if source != NodeRef::Local {
        crate::cloudflare_router::push_project_meta_to_agent(
            source.node_id(),
            &state.db,
            &state.node_clients,
            &state.config,
        )
        .await;
    }

    let size = std::fs::metadata(&out).map(|m| m.len()).ok();
    let artifact = out.file_name().map(|n| n.to_string_lossy().into_owned());
    logs(state, project_id, &format!("export complete: {}", artifact.as_deref().unwrap_or_default()));

    if let Some(mut job) = state.export_jobs.get_mut(job_id) {
        job.status = "completed".into();
        job.artifact = artifact;
        job.size_bytes = size;
        job.finished_at = Some(chrono::Utc::now().timestamp());
    }
    Ok(())
}

/// Spool a transfer stream to a temp file (bundle entries need sizes up front).
async fn spool_stream(
    stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = bytes::Bytes> + Send>>,
    tmp_dir: &std::path::Path,
    stem: &str,
) -> anyhow::Result<PathBuf> {
    use futures_util::StreamExt;
    use tokio_util::io::StreamReader;

    let path = tmp_dir.join(format!("{}-{}.tar", stem, uuid::Uuid::new_v4().simple()));
    let mut reader = StreamReader::new(stream.map(std::io::Result::Ok));
    let mut file = tokio::fs::File::create(&path).await?;
    tokio::io::copy(&mut reader, &mut file).await?;
    file.sync_all().await?;
    Ok(path)
}
