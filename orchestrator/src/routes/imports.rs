//! Import projects from export bundles.
//!
//! Bundles arrive two ways: uploaded straight through the dashboard (raw
//! streamed body) or scp'd into the server's imports folder and referenced by
//! file name — no dashboard needed (`curl POST /imports` works). Import
//! supports a new id (auto-suffixed on collision), re-scopes volumes to the
//! new namespace, and starts the project when asked. The archive keeps the
//! ORIGINAL id; all renaming happens here, on the importing master.

use std::path::PathBuf;
use std::pin::Pin;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::copy_engine::{self, NodeRef, bundle, manifest};
use crate::status::{self, ProjectUpdateFields};
use litebin_common::types::{DeployType, Node, ProjectStatus};

use super::exports::valid_file_name;
use crate::routes::manage::handlers::start_project;

type ByteStream = Pin<Box<dyn Stream<Item = Bytes> + Send>>;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ImportArchiveSummary {
    pub file: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ImportBundleRequest {
    /// Where the bundle lives: already in the imports folder (scp'd), or
    /// uploaded there via POST /imports/upload.
    pub file: String,
    /// New project id; defaults to the archive's original id. Auto-suffixed when taken.
    pub new_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Defaults to `local`.
    #[serde(default = "default_local")]
    pub target_node_id: String,
    /// Apply .env from the bundle (no-op when the bundle was exported without it).
    #[serde(default = "default_true")]
    pub include_env: bool,
    /// Restore volume data (volume config is always applied).
    #[serde(default = "default_true")]
    pub include_volumes: bool,
    #[serde(default = "default_true")]
    pub start_after_import: bool,
    #[serde(default)]
    pub delete_after_import: bool,
}

fn default_true() -> bool {
    true
}

fn default_local() -> String {
    "local".to_string()
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UploadQuery {
    pub name: Option<String>,
}

/// Resolve a request file name to a path inside the imports folder, or None
/// when the name is unsafe.
fn import_path(name: &str) -> Option<PathBuf> {
    if !valid_file_name(name) {
        return None;
    }
    let dir = copy_engine::imports_dir();
    let path = dir.join(name);
    match std::path::Path::new(&path).canonicalize() {
        Ok(p) => p.starts_with(dir.canonicalize().ok()?).then_some(p),
        // File may not exist yet (upload target) — the parent must resolve inside the dir.
        Err(_) => {
            let parent = path.parent()?;
            let parent_ok =
                parent.canonicalize().ok().zip(dir.canonicalize().ok()).is_some_and(|(p, d)| p.starts_with(d));
            parent_ok.then_some(path)
        }
    }
}

#[utoipa::path(
    get,
    path = "/imports",
    description = "List export bundles waiting in the server's imports folder.",
    responses((status = 200, body = serde_json::Value)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn list_import_archives(State(state): State<AppState>) -> impl IntoResponse {
    let mut archives: Vec<ImportArchiveSummary> = Vec::new();
    let dir = copy_engine::imports_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_file_name(&name)
                || !(name.ends_with(".l8b.tar.gz") || name.ends_with(".tar.gz") || name.ends_with(".tar"))
            {
                continue;
            }
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                archives.push(ImportArchiveSummary {
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
    archives.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let folder_path = state.docker.host_bind_source("/app/data/imports").await;
    (StatusCode::OK, Json(json!({ "archives": archives, "folder_path": folder_path })))
}

/// POST /imports/upload?name=foo.l8b.tar.gz — raw streamed body into the
/// imports folder. Giant bundles should go via scp instead.
#[utoipa::path(
    post,
    path = "/imports/upload",
    description = "Upload an export bundle into the imports folder (streamed raw body). For very large bundles, scp into the imports folder instead.",
    params(("name" = Option<String>, Query, description = "Destination file name")),
    responses((status = 200, body = serde_json::Value), (status = 400)),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn upload_import_archive(
    State(_state): State<AppState>,
    Query(q): Query<UploadQuery>,
    body: axum::body::Body,
) -> impl IntoResponse {
    let raw = q.name.unwrap_or_else(|| format!("upload-{}.l8b.tar.gz", chrono::Utc::now().format("%Y%m%d-%H%M%S")));
    let sanitized: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let Some(path) = import_path(&sanitized) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid file name"}))).into_response();
    };
    let mut part = path.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);

    use futures_util::StreamExt;
    use tokio_util::io::StreamReader;
    let stream = body.into_data_stream().map(|c| c.map_err(std::io::Error::other));
    let mut reader = StreamReader::new(stream);
    match tokio::fs::File::create(&part).await {
        Ok(mut file) => match tokio::io::copy(&mut reader, &mut file).await {
            Ok(_) => {
                drop(file);
                if let Err(e) = tokio::fs::rename(&part, &path).await {
                    let _ = tokio::fs::remove_file(&part).await;
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("upload failed: {e}")})))
                        .into_response();
                }
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("upload failed: {e}")})))
                    .into_response();
            }
        },
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("upload failed: {e}")})))
                .into_response();
        }
    }

    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(sanitized);
    (StatusCode::OK, Json(json!({ "file": name }))).into_response()
}

#[utoipa::path(
    post,
    path = "/imports",
    request_body = ImportBundleRequest,
    description = "Import a project from an export bundle in the imports folder: validate the manifest, allocate a fresh id (auto-suffix on collision), re-scope volumes, restore files/volumes/images, and start. Works via curl without the dashboard.",
    responses(
        (status = 202, description = "Import started in background", body = serde_json::Value),
        (status = 400, description = "Bad bundle, invalid id, or arch mismatch with embedded images"),
        (status = 404, description = "Bundle or target node not found"),
    ),
    tag = "manage",
    security(("session_auth" = []))
)]
pub async fn import_bundle(
    State(state): State<AppState>,
    Json(payload): Json<ImportBundleRequest>,
) -> impl IntoResponse {
    let Some(path) = import_path(&payload.file) else {
        return err(StatusCode::BAD_REQUEST, "invalid bundle file name".into());
    };
    if !path.exists() {
        return err(StatusCode::NOT_FOUND, format!("bundle '{}' not found in imports folder", payload.file));
    }

    let m = match bundle::read_manifest(&path).await {
        Ok(m) => m,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("not a LiteBin export bundle: {e}")),
    };
    if m.version > manifest::MANIFEST_VERSION {
        return err(
            StatusCode::BAD_REQUEST,
            format!("bundle was exported by a newer LiteBin (manifest v{}) — upgrade first", m.version),
        );
    }

    if payload.target_node_id != "local" {
        match sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&payload.target_node_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(n)) if n.status == litebin_common::types::NodeStatus::Online => {}
            Ok(Some(n)) => return err(StatusCode::BAD_REQUEST, format!("target node '{}' is not online", n.id)),
            Ok(None) => {
                return err(StatusCode::NOT_FOUND, format!("target node '{}' not found", payload.target_node_id));
            }
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
        }
    }
    let target_ref =
        if payload.target_node_id == "local" { NodeRef::Local } else { NodeRef::Agent(payload.target_node_id.clone()) };

    let mut warnings: Vec<String> = Vec::new();
    let target_arch = target_ref.architecture(&state).await;
    match (&m.source.architecture, &target_arch) {
        (Some(a), Some(b)) if a != b => {
            if !m.images.is_empty() {
                return err(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "source and target architectures differ ({a} vs {b}) and the bundle has uploaded images — \
                         redeploy from source on the target node instead"
                    ),
                );
            }
            warnings
                .push("source and target architectures differ — registry images must be multi-arch to start".into());
        }
        _ => {}
    }

    let reserved = [state.platform.dashboard_subdomain(), state.platform.poke_subdomain()];
    let base = m.project.id.clone();
    let name = payload.name.clone().or_else(|| m.project.name.clone());
    let description = payload.description.clone().or_else(|| m.project.description.clone());

    // Allocate + claim the id. A concurrent import can steal it between the
    // check and the INSERT; the unique constraint surfaces that race and the
    // loop retries with a fresh suffix. The projects INSERT runs first, so a
    // failed attempt leaves no partial rows behind.
    let mut requested = payload.new_id.clone();
    let mut alloc = None;
    for _ in 0..3 {
        let candidate = match copy_engine::allocate_project_id(&state.db, &reserved, requested.as_deref(), &base).await
        {
            Ok(a) => a,
            Err(e) => return err(StatusCode::CONFLICT, e.to_string()),
        };
        if candidate.suffixed {
            let original = requested.clone().unwrap_or_else(|| base.clone());
            warnings.push(format!("'{original}' was taken"));
        }
        match insert_from_manifest(
            &state,
            &m,
            &candidate.id,
            name.as_deref(),
            description.as_deref(),
            &payload.target_node_id,
            &mut warnings,
        )
        .await
        {
            Ok(()) => {
                alloc = Some(candidate);
                break;
            }
            Err(e) => {
                let raced = e
                    .root_cause()
                    .downcast_ref::<sqlx::Error>()
                    .map(crate::validation::is_unique_constraint)
                    .unwrap_or(false);
                if !raced {
                    return err(StatusCode::INTERNAL_SERVER_ERROR, format!("db insert: {e}"));
                }
                requested = Some(candidate.id);
            }
        }
    }
    let Some(alloc) = alloc else {
        return err(StatusCode::CONFLICT, "id kept colliding — retry the import".into());
    };

    let job = ImportJob {
        bundle_path: path.clone(),
        old_id: m.project.id.clone(),
        new_id: alloc.id.clone(),
        target: target_ref,
        include_env: payload.include_env && m.flags.include_env,
        include_volumes: payload.include_volumes,
        start_after: payload.start_after_import,
        images: m.images.iter().map(|i| (i.file.clone(), i.r#ref.clone())).collect(),
    };
    let state_bg = state.clone();
    let new_id = alloc.id.clone();
    let delete_after = payload.delete_after_import;
    tokio::spawn(async move {
        let result = run_import(&state_bg, &job).await;
        if delete_after && result.is_ok() {
            let _ = tokio::fs::remove_file(&job.bundle_path).await;
        }
        if let Err(e) = result {
            tracing::error!(project = %new_id, error = %e, "import failed");
            crate::routes::deploy::logs::push_deploy_log(&state_bg, &new_id, &format!("import failed: {e}"));
            let _ =
                status::transition(&state_bg.db, &new_id, ProjectStatus::Error, &ProjectUpdateFields::default(), None)
                    .await;
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "project_id": alloc.id,
            "status": "importing",
            "warnings": warnings,
        })),
    )
        .into_response()
}

fn err(status: StatusCode, message: String) -> axum::response::Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn insert_from_manifest(
    state: &AppState,
    m: &manifest::ExportManifest,
    new_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    target_node_id: &str,
    warnings: &mut Vec<String>,
) -> anyhow::Result<()> {
    // Attribute to the first user (manage tokens have no user; single-master
    // installs are effectively single-owner).
    let user_id: String = sqlx::query_scalar("SELECT id FROM users ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "system".to_string());

    // The custom domain travels with the bundle (user-owned DNS — re-point it
    // at this master); skip when another project already claims it here.
    let custom_domain = match m.project.custom_domain.as_deref() {
        Some(d) if !d.is_empty() => {
            let claimed: Option<String> =
                sqlx::query_scalar("SELECT id FROM projects WHERE custom_domain = ? AND id != ?")
                    .bind(d)
                    .bind(new_id)
                    .fetch_optional(&state.db)
                    .await?;
            if claimed.is_some() {
                warnings.push(format!("custom domain '{d}' is already claimed by another project; skipped"));
                None
            } else {
                Some(d.to_string())
            }
        }
        _ => None,
    };

    let deploy_type = match m.project.deploy_type.as_deref() {
        Some("compose") => DeployType::Compose,
        _ => DeployType::Image,
    };
    let now = chrono::Utc::now().timestamp();
    let node_id_for_db = if target_node_id == "local" { None } else { Some(target_node_id) };

    sqlx::query(
        "INSERT INTO projects (id, user_id, name, description, is_background, image, internal_port, \
         status, node_id, cmd, memory_limit_mb, cpu_limit, auto_stop_enabled, auto_stop_timeout_mins, \
         auto_start_enabled, allow_raw_ports, allow_docker_access, volumes, deploy_type, custom_domain, \
         created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id)
    .bind(user_id)
    .bind(name)
    .bind(description)
    .bind(m.project.is_background)
    .bind(&m.project.image)
    .bind(m.project.internal_port)
    .bind(ProjectStatus::Importing)
    .bind(node_id_for_db)
    .bind(&m.project.cmd)
    .bind(m.project.memory_limit_mb)
    .bind(m.project.cpu_limit)
    .bind(m.project.auto_stop_enabled)
    .bind(m.project.auto_stop_timeout_mins)
    .bind(m.project.auto_start_enabled)
    .bind(m.project.allow_raw_ports)
    .bind(m.project.allow_docker_access)
    .bind(&m.project.volumes)
    .bind(deploy_type)
    .bind(custom_domain)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    for svc in &m.services {
        sqlx::query(
            "INSERT INTO project_services (project_id, service_name, image, port, cmd, is_public, depends_on, \
             memory_limit_mb, cpu_limit, status, is_oneshot) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'stopped', ?)",
        )
        .bind(new_id)
        .bind(&svc.service_name)
        .bind(&svc.image)
        .bind(svc.port)
        .bind(&svc.cmd)
        .bind(svc.is_public)
        .bind(&svc.depends_on)
        .bind(svc.memory_limit_mb)
        .bind(svc.cpu_limit)
        .bind(svc.is_oneshot)
        .execute(&state.db)
        .await?;
    }

    for vol in &m.volumes {
        sqlx::query(
            "INSERT INTO project_volumes (project_id, service_name, volume_name, container_path) VALUES (?, ?, ?, ?)",
        )
        .bind(new_id)
        .bind(&vol.service_name)
        .bind(copy_engine::rescope_volume(&vol.scoped_name, &m.project.id, new_id))
        .bind(&vol.container_path)
        .execute(&state.db)
        .await?;
    }

    // Path routes copy as-is; alias subdomains claimed unless already taken.
    for r in &m.routes {
        if r.route_type == "alias"
            && let Some(sub) = r.subdomain.as_deref()
        {
            let alias_taken: Option<String> = sqlx::query_scalar(
                "SELECT subdomain FROM project_routes WHERE route_type = 'alias' AND subdomain = ? LIMIT 1",
            )
            .bind(sub)
            .fetch_optional(&state.db)
            .await?;
            let project_taken: Option<String> =
                sqlx::query_scalar("SELECT id FROM projects WHERE id = ?").bind(sub).fetch_optional(&state.db).await?;
            if alias_taken.is_some() || project_taken.is_some() {
                warnings.push(format!("alias '{sub}' is taken on this master; skipped"));
                continue;
            }
        }
        sqlx::query(
            "INSERT INTO project_routes (id, project_id, route_type, path, subdomain, upstream, priority, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(new_id)
        .bind(&r.route_type)
        .bind(&r.path)
        .bind(&r.subdomain)
        .bind(&r.upstream)
        .bind(r.priority)
        .bind(now)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

fn file_stream(path: &std::path::Path) -> ByteStream {
    use futures_util::StreamExt;
    let path = path.to_path_buf();
    let stream = futures_util::stream::once(async move {
        match tokio::fs::File::open(&path).await {
            Ok(f) => Box::pin(tokio_util::io::ReaderStream::with_capacity(f, 1024 * 1024))
                as Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
            // A failed open yields an empty stream; the transfer call surfaces the real error.
            Err(_) => Box::pin(tokio_util::io::ReaderStream::new(tokio::io::empty())),
        }
    })
    .flatten()
    .map(|c| c.unwrap_or_default());
    Box::pin(stream)
}

/// Everything the background import task needs, bundled for run_import.
struct ImportJob {
    bundle_path: PathBuf,
    old_id: String,
    new_id: String,
    target: NodeRef,
    include_env: bool,
    include_volumes: bool,
    start_after: bool,
    images: std::collections::HashMap<String, String>,
}

async fn run_import(state: &AppState, job: &ImportJob) -> anyhow::Result<()> {
    let logs = crate::routes::deploy::logs::push_deploy_log;
    let tmp = copy_engine::spool_dir(state);
    logs(state, &job.new_id, &format!("importing from bundle (originally '{}')", job.old_id));

    let state_cb = state.clone();
    let target_cb = job.target.clone();
    let old_id_cb = job.old_id.clone();
    let new_id_cb = job.new_id.clone();
    let include_env_cb = job.include_env;
    let include_volumes_cb = job.include_volumes;
    let images_cb = job.images.clone();
    let tmp_cb = tmp.clone();

    let on_entry = move |name: String, spool: PathBuf| {
        let state = state_cb.clone();
        let target = target_cb.clone();
        let old_id = old_id_cb.clone();
        let new_id = new_id_cb.clone();
        let include_env = include_env_cb;
        let include_volumes = include_volumes_cb;
        let images = images_cb.clone();
        let tmp = tmp_cb.clone();
        async move {
            let result: anyhow::Result<()> = async {
                if name == bundle::PROJECT_ENTRY {
                    let raw = file_stream(&spool);
                    let tar = copy_engine::reroot::reroot(raw, &old_id, &new_id, include_env, &tmp).await?;
                    target
                        .transfer_in(&state, &litebin_common::docker::TransferKind::ProjectDir(new_id.clone()), tar)
                        .await?;
                    logs(&state, &new_id, "project files restored");
                } else if let Some(scoped) = name.strip_prefix("volumes/").and_then(|n| n.strip_suffix(".tar")) {
                    // Relative binds ride in project.tar — a volumes/ entry for
                    // one only appears in hand-made bundles.
                    let is_bind = litebin_common::types::classify_volume(scoped)
                        == litebin_common::types::VolumeKind::RelativeBindMount;
                    if include_volumes && !is_bind {
                        let to = copy_engine::rescope_volume(scoped, &old_id, &new_id);
                        let tar = file_stream(&spool);
                        target.transfer_in(&state, &litebin_common::docker::TransferKind::Volume(to), tar).await?;
                        logs(&state, &new_id, &format!("volume '{scoped}' restored"));
                    }
                } else if let Some(image) = name.strip_prefix("images/").and_then(|n| images.get(n)) {
                    let tar = file_stream(&spool);
                    target
                        .transfer_in(&state, &litebin_common::docker::TransferKind::Image(image.clone()), tar)
                        .await?;
                    logs(&state, &new_id, "uploaded image restored");
                } else {
                    tracing::warn!(entry = %name, "import: skipping unknown bundle entry");
                }
                Ok(())
            }
            .await;
            let _ = tokio::fs::remove_file(&spool).await;
            result
        }
    };

    bundle::iterate_entries(&job.bundle_path, &tmp, on_entry).await?;

    status::transition(&state.db, &job.new_id, ProjectStatus::Stopped, &ProjectUpdateFields::default(), None).await?;
    if job.start_after {
        logs(state, &job.new_id, "starting");
        match start_project(axum::extract::State(state.clone()), axum::extract::Path(job.new_id.to_string())).await {
            Ok(_) => logs(state, &job.new_id, "started"),
            Err((code, message)) => anyhow::bail!("start failed ({code}): {message}"),
        }
    }

    if job.target != NodeRef::Local {
        crate::cloudflare_router::push_project_meta_to_agent(
            job.target.node_id(),
            &state.db,
            &state.node_clients,
            &state.config,
        )
        .await;
    }
    let _ = state.route_sync_tx.send(());
    logs(state, &job.new_id, "import complete");
    Ok(())
}
