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

/// A user-named restore point — a bookmark on the continuous restore window.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BackupPoint {
    pub id: i64,
    pub name: String,
    /// Bookmarked moment (RFC 3339) — passed to `litestream restore -timestamp`.
    pub timestamp: String,
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
    /// Local replica store summary — None until litestream creates it.
    pub replica: Option<crate::backup::ReplicaInfo>,
    /// User-named restore points, newest first.
    pub points: Vec<BackupPoint>,
    pub s3: BackupS3Status,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupS3Request {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct BackupRestoreRequest {
    /// Point-in-time restore (RFC 3339). Latest snapshot when omitted.
    pub timestamp: Option<String>,
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
    Ok(rows.into_iter().map(|(id, name, timestamp)| BackupPoint { id, name, timestamp }).collect())
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

    let replica_dir = data_dir.as_deref().map(|dir| dir.join("backups").join("litebin.db"));
    let replica = replica_dir.as_deref().and_then(backup::replica_info);
    let last_sync = replica.as_ref().and_then(|r| r.last_write.clone());
    let db_size_bytes =
        data_dir.as_deref().and_then(|dir| std::fs::metadata(dir.join("litebin.db")).ok()).map(|m| m.len());

    Ok(BackupStatus {
        enabled,
        available,
        sidecar_installed: installed,
        sidecar_running: running,
        last_sync,
        db_size_bytes,
        replica,
        points: load_points(state).await?,
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

#[utoipa::path(
    get,
    path = "/backup/status",
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
    request_body = BackupS3Request,
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 400, description = "invalid S3 config"),
        (status = 409, description = "backup not enabled"),
    ),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_add_s3(
    State(state): State<AppState>,
    Json(payload): Json<BackupS3Request>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
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

    upsert_setting(&state.db, SETTING_S3_ENDPOINT, &endpoint).await?;
    upsert_setting(&state.db, SETTING_S3_BUCKET, &bucket).await?;
    upsert_setting(&state.db, SETTING_S3_PREFIX, &prefix).await?;
    upsert_setting(&state.db, SETTING_S3_ACCESS_KEY, payload.access_key_id.trim()).await?;
    upsert_setting(&state.db, SETTING_S3_SECRET_KEY, payload.secret_access_key.trim()).await?;

    sync_sidecar_config(&state).await?;
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    delete,
    path = "/backup/s3",
    responses((status = 200, body = BackupActionResponse)),
    tag = "backup",
    security(("session_auth" = []))
)]
pub async fn backup_remove_s3(
    State(state): State<AppState>,
) -> Result<Json<BackupActionResponse>, (StatusCode, String)> {
    for key in [SETTING_S3_ENDPOINT, SETTING_S3_BUCKET, SETTING_S3_PREFIX, SETTING_S3_ACCESS_KEY, SETTING_S3_SECRET_KEY]
    {
        upsert_setting(&state.db, key, "").await?;
    }
    sync_sidecar_config(&state).await?;
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/points",
    request_body = BackupPointRequest,
    responses(
        (status = 200, body = BackupActionResponse),
        (status = 400, description = "invalid name"),
        (status = 409, description = "no backup data yet"),
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

    // The point must land inside the restore window — require existing replica data.
    let data_dir = backup::resolve_data_dir(&state.config.database_url)
        .ok_or((StatusCode::CONFLICT, "backup unavailable: in-memory database".into()))?;
    let has_data = backup::replica_info(&data_dir.join("backups").join("litebin.db")).is_some_and(|r| r.file_count > 0);
    if !has_data {
        return Err((StatusCode::CONFLICT, "no backup data yet — wait for the first sync".into()));
    }

    sqlx::query("INSERT INTO backup_points (name, timestamp) VALUES (?, ?)")
        .bind(&name)
        .bind(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    delete,
    path = "/backup/points/{id}",
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
    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: false }))
}

#[utoipa::path(
    post,
    path = "/backup/restore",
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

    // The restored snapshot lands beside the live DB; the swap happens at boot.
    // Exit after the response flushes — the restart policy brings us back.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tracing::info!("restore staged — restarting orchestrator to apply");
        std::process::exit(0);
    });

    Ok(Json(BackupActionResponse { status: load_status(&state).await?, restarting: true }))
}
