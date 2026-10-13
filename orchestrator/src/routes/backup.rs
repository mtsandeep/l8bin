//! Backup admin endpoints: status, enable/disable, S3 replica config, restore.

use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::backup::{
    self, SETTING_BACKUP_ENABLED, SETTING_S3_ACCESS_KEY, SETTING_S3_BUCKET, SETTING_S3_ENDPOINT, SETTING_S3_PREFIX,
    SETTING_S3_SECRET_KEY, SIDECAR_CONTAINER_NAME,
};

use super::global_settings::{get_setting, upsert_setting};

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BackupS3Status {
    pub configured: bool,
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub prefix: Option<String>,
}

/// A user-named restore point — a `VACUUM INTO` snapshot of the live database,
/// kept as a local file so it survives replica switches and disabled backup.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BackupPoint {
    pub id: i64,
    pub name: String,
    /// When the point was taken (RFC 3339).
    pub timestamp: String,
    pub size_bytes: u64,
    /// Snapshot file present. Rows from before the file-based points have none.
    pub available: bool,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BackupStatus {
    /// Backup turned on (sidecar should be running).
    pub enabled: bool,
    /// Orchestrator data storage can be shared with a sidecar (running in a container).
    pub available: bool,
    pub sidecar_installed: bool,
    pub sidecar_running: bool,
    /// Last local replica write time (RFC 3339), None before the first sync.
    pub last_sync: Option<String>,
    /// Live database size in bytes.
    pub db_size_bytes: Option<u64>,
    /// Active replica store summary (local file store, or the s3/r2 staging store).
    pub replica: Option<crate::backup::ReplicaInfo>,
    /// Earliest restorable moment (RFC 3339) across the active replica.
    pub coverage_start: Option<String>,
    /// Local daily snapshots (s3/r2 mode), newest first.
    pub dailies: Vec<crate::backup::DailySnapshot>,
    /// User-named restore points, newest first.
    pub points: Vec<BackupPoint>,
    /// When the kept pre-restore database was last live (RFC 3339) — the undo
    /// target. None when no restore has been applied.
    pub revertible_to: Option<String>,
    /// Last s3/r2 destination health probe (None before the first check).
    pub destination_health: Option<crate::backup::DestinationHealth>,
    pub s3: BackupS3Status,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupS3Request {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Proceed even though the destination prefix already holds backup files
    /// (LiteBin starts a new backup alongside them; old files are left alone).
    #[serde(default)]
    pub confirm_existing: bool,
}

/// Response for `POST /backup/s3` — `existing_files` is set when the destination
/// prefix already holds files and nothing was saved (re-submit to proceed).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BackupS3Response {
    pub status: BackupStatus,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub existing_files: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupRestoreRequest {
    /// Point-in-time restore (RFC 3339). Latest snapshot when omitted.
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupRestoreLocalRequest {
    /// Daily snapshot date (YYYY-MM-DD).
    pub date: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupRestorePointRequest {
    pub point_id: i64,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupPointRequest {
    /// Label for the current moment (1-64 chars).
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BackupActionResponse {
    pub status: BackupStatus,
    /// Set when the orchestrator is about to restart (restore applied).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restarting: bool,
}

async fn load_s3(state: &AppState) -> Result<backup::S3Config, (StatusCode, String)> {
    Ok(backup::S3Config {
        endpoint: get_setting(&state.db, SETTING_S3_ENDPOINT).await?.unwrap_or_default(),
        bucket: get_setting(&state.db, SETTING_S3_BUCKET).await?.unwrap_or_default(),
        prefix: get_setting(&state.db, SETTING_S3_PREFIX).await?.unwrap_or_default(),
        access_key_id: get_setting(&state.db, SETTING_S3_ACCESS_KEY).await?.unwrap_or_default(),
        secret_access_key: get_setting(&state.db, SETTING_S3_SECRET_KEY).await?.unwrap_or_default(),
    })
}

async fn load_points(state: &AppState) -> Result<Vec<BackupPoint>, (StatusCode, String)> {
    let rows = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, name, timestamp FROM backup_points ORDER BY timestamp DESC, id DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let data_dir = backup::resolve_data_dir(&state.config.database_url);
    Ok(rows
        .into_iter()
        .map(|(id, name, timestamp)| {
            let file = data_dir.as_deref().map(|dir| backup::point_file(dir, id));
            let size_bytes = file.as_deref().and_then(|f| std::fs::metadata(f).ok()).map(|m| m.len()).unwrap_or(0);
            BackupPoint { id, name, timestamp, size_bytes, available: size_bytes > 0 }
        })
        .collect())
}

async fn load_status(state: &AppState) -> Result<BackupStatus, (StatusCode, String)> {
    let enabled = get_setting(&state.db, SETTING_BACKUP_ENABLED).await?.as_deref() == Some("1");
    let s3 = load_s3(state).await?;

    let data_dir = backup::resolve_data_dir(&state.config.database_url);
    let available = match &data_dir {
        Some(dir) => state.docker.detect_mount_bind_source(&dir.to_string_lossy()).await.is_some(),
        None => false,
    };

    let installed = matches!(state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await, Ok(Some(_)));
    let running = match state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await {
        Ok(Some(id)) => state.docker.is_container_running(&id).await.unwrap_or(false),
        _ => false,
    };

    // Active-replica stats: the local file store, or litestream's staging store for
    // the s3/r2 replica (same LTX chain it uploads from).
    let replica = match data_dir.as_deref() {
        Some(dir) if s3.is_configured() => backup::replica_info(&dir.join(".litebin.db-litestream")),
        Some(dir) => backup::replica_info(&dir.join("backups").join("litebin.db")),
        None => None,
    };
    let last_sync = replica.as_ref().and_then(|r| r.last_write.clone());
    let db_size_bytes =
        data_dir.as_deref().and_then(|dir| std::fs::metadata(dir.join("litebin.db")).ok()).map(|m| m.len());

    let s3_since = if s3.is_configured() {
        get_setting(&state.db, backup::SETTING_S3_SINCE).await?.filter(|v| !v.is_empty())
    } else {
        None
    };
    let coverage_start = replica.as_ref().and_then(|r| r.coverage_start.clone()).or(s3_since);
    let dailies = if s3.is_configured() {
        data_dir.as_deref().map(backup::daily_snapshots).unwrap_or_default()
    } else {
        Vec::new()
    };
    let destination_health = state.destination_health.read().expect("destination health lock").clone();
    let revertible_to = data_dir.as_deref().and_then(|dir| {
        std::fs::metadata(dir.join("litebin.db.pre-restore"))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(backup::system_time_to_rfc3339)
    });

    Ok(BackupStatus {
        enabled,
        available,
        sidecar_installed: installed,
        sidecar_running: running,
        last_sync,
        db_size_bytes,
        replica,
        coverage_start,
        dailies,
        points: load_points(state).await?,
        revertible_to,
        destination_health,
        s3: BackupS3Status {
            configured: s3.is_configured(),
            endpoint: (!s3.endpoint.is_empty()).then_some(s3.endpoint),
            bucket: (!s3.bucket.is_empty()).then_some(s3.bucket),
            prefix: (!s3.prefix.is_empty()).then_some(s3.prefix),
        },
    })
}

/// Write litestream.yml from current settings and (re)start the sidecar so it
/// picks the new config up. Best-effort restart — a fresh create also re-reads.
async fn sync_sidecar_config(state: &AppState) -> Result<(), (StatusCode, String)> {
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let s3 = load_s3(state).await?;
    backup::write_litestream_yml(&data_dir, &s3)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write litestream.yml: {e}")))?;
    if let Some(id) = state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await.map_err(internal)? {
        state.docker.restart_container(&id).await.map_err(internal)?;
    }
    Ok(())
}

fn internal(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Checkpoint the live WAL so the db file moved aside at swap time is self-contained.
async fn checkpoint_live_db(state: &AppState) -> Result<(), (StatusCode, String)> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("checkpoint: {e}")))?;
    Ok(())
}

#[utoipa::path(
    get,
    path = "/backup/status",
    description = "Backup status: sidecar state, replica stores, snapshots, named points, and undo target.",
    responses((status = 200, body = BackupStatus)),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_status(State(state): State<AppState>) -> Result<Json<BackupStatus>, (StatusCode, String)> {
    Ok(Json(load_status(&state).await?))
}

#[utoipa::path(
    post,
    path = "/backup/enable",
    description = "Turn on the Litestream backup sidecar (local replica; add an s3/r2 destination for continuous off-site).",
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 409, description = "backup unavailable (not running in a container / in-memory db)"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_enable(State(state): State<AppState>) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let bind = state.docker.detect_mount_bind_source(&data_dir.to_string_lossy()).await.ok_or((
        StatusCode::CONFLICT,
        "backup unavailable: no shared data mount detected (is the orchestrator running in a container?)".into(),
    ))?;

    let s3 = load_s3(&state).await?;
    backup::write_litestream_yml(&data_dir, &s3)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write litestream.yml: {e}")))?;
    state
        .docker
        .ensure_sidecar_container(&backup::sidecar_spec(state.docker.network(), &bind))
        .await
        .map_err(internal)?;
    upsert_setting(&state.db, SETTING_BACKUP_ENABLED, "1").await?;

    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/disable",
    description = "Stop the backup sidecar and turn backup off; existing replica data is kept.",
    responses((status = 200, body = BackupActionResponse)),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_disable(State(state): State<AppState>) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    state.docker.remove_sidecar_container(SIDECAR_CONTAINER_NAME).await.map_err(internal)?;
    upsert_setting(&state.db, SETTING_BACKUP_ENABLED, "0").await?;
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/s3",
    description = "Configure the s3/r2 destination (validated with a probe via the running sidecar); it becomes the primary replica.",
    request_body = BackupS3Request,
    responses(
        (status = 200, body = BackupS3Response),
        (status = 400, description = "invalid config or destination probe failed"),
        (status = 409, description = "backup not enabled / sidecar not running"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_add_s3(
    State(state): State<AppState>,
    Json(payload): Json<BackupS3Request>,
) -> Result<Json<BackupS3Response>, (StatusCode, String)> {
    let endpoint = payload.endpoint.trim().to_string();
    let bucket = payload.bucket.trim().to_string();
    if endpoint.is_empty() || bucket.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "endpoint and bucket are required".into()));
    }
    if payload.access_key_id.trim().is_empty() || payload.secret_access_key.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "access_key_id and secret_access_key are required".into()));
    }
    if get_setting(&state.db, SETTING_BACKUP_ENABLED).await?.as_deref() != Some("1") {
        return Err((StatusCode::CONFLICT, "enable backup before configuring S3".into()));
    }
    let prefix = payload.prefix.unwrap_or_else(|| "litebin/".into()).trim().trim_matches('/').to_string();

    // Validate the destination before saving anything: probe via the running sidecar.
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    match state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await {
        Ok(Some(id)) if state.docker.is_container_running(&id).await.unwrap_or(false) => {}
        _ => return Err((StatusCode::CONFLICT, "backup sidecar is not running".into())),
    }
    let candidate = backup::S3Config {
        endpoint: endpoint.clone(),
        bucket: bucket.clone(),
        prefix: prefix.clone(),
        access_key_id: payload.access_key_id.trim().to_string(),
        secret_access_key: payload.secret_access_key.trim().to_string(),
    };
    let existing = match backup::probe_destination(&state.docker, &data_dir, &candidate).await {
        Ok(backup::DestinationProbe::Existing) => true,
        Ok(backup::DestinationProbe::Empty) => false,
        Ok(backup::DestinationProbe::Failed(detail)) => {
            tracing::warn!(detail = %detail, "backup: destination probe failed");
            return Err((StatusCode::BAD_REQUEST, format!("destination check failed: {detail}")));
        }
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("destination probe: {e}"))),
    };
    // A credential-only update on the same destination resumes the existing store —
    // the files found there are its own, no "alongside" decision needed.
    let current = load_s3(&state).await?;
    let rotation = backup::same_destination(&current, &candidate);
    tracing::info!(existing, rotation, endpoint = %endpoint, bucket = %bucket, prefix = %prefix, "backup: destination save preflight");
    if existing && !rotation && !payload.confirm_existing {
        return Ok(Json(BackupS3Response { status: load_status(&state).await?, existing_files: true }));
    }

    upsert_setting(&state.db, SETTING_S3_ENDPOINT, &endpoint).await?;
    upsert_setting(&state.db, SETTING_S3_BUCKET, &bucket).await?;
    upsert_setting(&state.db, SETTING_S3_PREFIX, &prefix).await?;
    upsert_setting(&state.db, SETTING_S3_ACCESS_KEY, payload.access_key_id.trim()).await?;
    upsert_setting(&state.db, SETTING_S3_SECRET_KEY, payload.secret_access_key.trim()).await?;
    if get_setting(&state.db, backup::SETTING_S3_SINCE).await?.is_none_or(|v| v.is_empty()) {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        upsert_setting(&state.db, backup::SETTING_S3_SINCE, &now).await?;
    }

    sync_sidecar_config(&state).await?;
    let checked_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    *state.destination_health.write().expect("destination health lock") =
        Some(backup::DestinationHealth { ok: true, checked_at, failing_since: None, detail: None });
    Ok(Json(BackupS3Response { status: load_status(&state).await?, existing_files: existing }))
}

#[utoipa::path(
    delete,
    path = "/backup/s3",
    description = "Remove the s3/r2 destination and fall back to the local replica.",
    responses((status = 200, body = BackupActionResponse)),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_remove_s3(
    State(state): State<AppState>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    for key in [
        SETTING_S3_ENDPOINT,
        SETTING_S3_BUCKET,
        SETTING_S3_PREFIX,
        SETTING_S3_ACCESS_KEY,
        SETTING_S3_SECRET_KEY,
        backup::SETTING_S3_SINCE,
    ] {
        upsert_setting(&state.db, key, "").await?;
    }
    sync_sidecar_config(&state).await?;
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/points",
    description = "Take a named restore point (VACUUM INTO snapshot kept as a local file).",
    request_body = BackupPointRequest,
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 400, description = "invalid name"),
        (status = 409, description = "in-memory database"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_create_point(
    State(state): State<AppState>,
    Json(payload): Json<BackupPointRequest>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let name = payload.name.trim().to_string();
    if name.is_empty() || name.len() > 64 {
        return Err((StatusCode::BAD_REQUEST, "name must be 1-64 characters".into()));
    }
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    backup::take_named_point(&state.db, &data_dir, &name).await.map_err(internal)?;
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    delete,
    path = "/backup/points/{id}",
    description = "Delete a named restore point and its snapshot file.",
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 404, description = "point not found"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_delete_point(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let result = sqlx::query("DELETE FROM backup_points WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "backup point not found".into()));
    }
    if let Some(dir) = backup::resolve_data_dir(&state.config.database_url) {
        let _ = std::fs::remove_file(backup::point_file(&dir, id));
    }
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/restore",
    description = "Point-in-time restore from the replica (latest snapshot when no timestamp given); the orchestrator restarts to apply it.",
    request_body = BackupRestoreRequest,
    responses(
        (status = 200, body = BackupActionResponse, description = "restore staged; orchestrator restarting"),
        (status = 409, description = "backup sidecar not running"),
        (status = 500, description = "litestream restore failed"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_restore(
    State(state): State<AppState>,
    Json(payload): Json<BackupRestoreRequest>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let sidecar = match state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await {
        Ok(Some(id)) => id,
        Ok(None) => return Err((StatusCode::CONFLICT, "backup sidecar is not installed".into())),
        Err(e) => return Err((StatusCode::CONFLICT, format!("backup sidecar lookup failed: {e}"))),
    };
    if !state.docker.is_container_running(&sidecar).await.unwrap_or(false) {
        return Err((StatusCode::CONFLICT, "backup sidecar is not running".into()));
    }

    let mut cmd = vec![
        "litestream",
        "restore",
        "-config",
        backup::SIDECAR_CONFIG_PATH,
        "-o",
        "/data/litebin.db.restored",
        "-force",
    ];
    if let Some(ts) = payload.timestamp.as_deref().filter(|ts| !ts.trim().is_empty()) {
        cmd.push("-timestamp");
        cmd.push(ts.trim());
    }
    // litestream ≥0.5 requires a positional DB_PATH (config alone is not enough)
    cmd.push(backup::SIDECAR_DB_PATH);

    let exec = tokio::time::timeout(Duration::from_secs(300), state.docker.exec_container(&sidecar, &cmd))
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "litestream restore timed out".into()))?
        .map_err(internal)?;

    if exec.exit_code != 0 {
        tracing::error!(exit_code = exec.exit_code, output = %exec.output, "litestream restore failed");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("litestream restore failed: {}", exec.output)));
    }

    checkpoint_live_db(&state).await?;
    stage_restart();
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: true }))
}

#[utoipa::path(
    post,
    path = "/backup/revert-restore",
    description = "Undo the last applied restore by staging the kept pre-restore database back in.",
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 404, description = "no restore to undo"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_revert_restore(
    State(state): State<AppState>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let pre = data_dir.join("litebin.db.pre-restore");
    if !pre.is_file() {
        return Err((StatusCode::NOT_FOUND, "no restore to undo (litebin.db.pre-restore not found)".into()));
    }

    checkpoint_live_db(&state).await?;
    std::fs::copy(&pre, data_dir.join("litebin.db.restored"))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("copy pre-restore: {e}")))?;
    let pre_wal = data_dir.join("litebin.db.pre-restore-wal");
    if pre_wal.is_file() {
        let _ = std::fs::copy(&pre_wal, data_dir.join("litebin.db.restored-wal"));
    }
    tracing::info!("revert staged — restarting orchestrator to undo the last restore");
    stage_restart();
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: true }))
}

/// Exit after the response flushes — the restart policy brings us back and the
/// boot swap applies the staged restore.
fn stage_restart() {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tracing::info!("restore staged — restarting orchestrator to apply");
        std::process::exit(0);
    });
}

#[utoipa::path(
    post,
    path = "/backup/restore-local",
    description = "Restore from a local daily snapshot, bypassing the s3/r2 replica.",
    request_body = BackupRestoreLocalRequest,
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 400, description = "invalid date"),
        (status = 404, description = "no snapshot for that date"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_restore_local(
    State(state): State<AppState>,
    Json(payload): Json<BackupRestoreLocalRequest>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let date = payload.date.trim();
    let valid = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date.bytes().enumerate().all(|(i, b)| b.is_ascii_digit() || (i == 4 || i == 7) && b == b'-');
    if !valid {
        return Err((StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD".into()));
    }

    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let src = data_dir.join("backups").join("daily").join(format!("litebin-{date}.db"));
    if !src.is_file() {
        return Err((StatusCode::NOT_FOUND, format!("no local daily snapshot for {date}")));
    }
    stage_file_restore(&state, &src).await
}

#[utoipa::path(
    post,
    path = "/backup/restore-point",
    description = "Restore a named point from its local snapshot file.",
    request_body = BackupRestorePointRequest,
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 404, description = "point or its snapshot file not found"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_restore_point(
    State(state): State<AppState>,
    Json(payload): Json<BackupRestorePointRequest>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM backup_points WHERE id = ?")
        .bind(payload.point_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(name) = name else {
        return Err((StatusCode::NOT_FOUND, "backup point not found".into()));
    };

    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let src = backup::point_file(&data_dir, payload.point_id);
    if !src.is_file() {
        return Err((StatusCode::NOT_FOUND, format!("snapshot file for point \"{name}\" is missing")));
    }
    stage_file_restore(&state, &src).await
}

/// Copy a snapshot file next to the live DB as the pending restore; the swap
/// happens at boot. Exits after the response flushes — the restart policy
/// brings us back.
async fn stage_file_restore(
    state: &AppState,
    src: &std::path::Path,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    checkpoint_live_db(state).await?;
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let dest = data_dir.join("litebin.db.restored");
    std::fs::copy(src, &dest).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("copy snapshot: {e}")))?;

    stage_restart();
    Ok(Json(BackupActionResponse { status: load_status(state).await?, restarting: true }))
}
