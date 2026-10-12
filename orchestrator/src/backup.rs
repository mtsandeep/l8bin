//! Litestream backup sidecar: config generation, restore swap, and replica status.
//!
//! Backup is opt-in (dashboard/CLI `enable`), master-only. The orchestrator owns
//! `/app/data/litestream.yml` and manages the `litebin-backup` sidecar container
//! directly via the Docker API — no compose involvement.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::routes::global_settings::get_setting;

pub const SIDECAR_CONTAINER_NAME: &str = "litebin-backup";
/// When the s3/r2 destination was first configured (RFC 3339) — restore-coverage
/// floor for the remote replica.
pub const SETTING_S3_SINCE: &str = "backup_s3_since";
pub const LITESTREAM_IMAGE: &str = "litestream/litestream:0.5.17";

pub const SETTING_BACKUP_ENABLED: &str = "backup_enabled";
pub const SETTING_S3_ENDPOINT: &str = "backup_s3_endpoint";
pub const SETTING_S3_BUCKET: &str = "backup_s3_bucket";
pub const SETTING_S3_PREFIX: &str = "backup_s3_prefix";
pub const SETTING_S3_ACCESS_KEY: &str = "backup_s3_access_key";
pub const SETTING_S3_SECRET_KEY: &str = "backup_s3_secret_key";

/// Paths inside the sidecar container (data volume mounted at /data).
pub const SIDECAR_DB_PATH: &str = "/data/litebin.db";
pub const SIDECAR_CONFIG_PATH: &str = "/data/litestream.yml";
pub const SIDECAR_REPLICA_URL: &str = "file:///data/backups/litebin.db";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl S3Config {
    pub fn is_configured(&self) -> bool {
        !(self.endpoint.is_empty() || self.bucket.is_empty())
    }
}

/// Resolve the orchestrator's data directory from DATABASE_URL.
/// Returns None for in-memory databases (tests) — backup is unavailable there.
pub fn resolve_data_dir(database_url: &str) -> Option<PathBuf> {
    let path = database_url.strip_prefix("sqlite:")?;
    if path.contains(":memory:") {
        return None;
    }
    let dir = Path::new(path).parent()?;
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    // Normalize "./data" → "data" so the joined path matches container mount destinations.
    let dir = dir.strip_prefix(".").unwrap_or(dir);
    if dir.as_os_str().is_empty() {
        Some(std::env::current_dir().ok()?)
    } else if dir.is_absolute() {
        Some(dir.to_path_buf())
    } else {
        Some(std::env::current_dir().ok()?.join(dir))
    }
}

/// Build the sidecar spec for the backup container. `bind` is the volume name or
/// host path backing the orchestrator's data directory.
pub fn sidecar_spec(network: &str, bind: &str) -> litebin_common::docker::SidecarSpec {
    litebin_common::docker::SidecarSpec {
        name: SIDECAR_CONTAINER_NAME.to_string(),
        image: LITESTREAM_IMAGE.to_string(),
        binds: vec![format!("{bind}:/data")],
        cmd: vec!["replicate".into(), "-config".into(), SIDECAR_CONFIG_PATH.into()],
        network: network.to_string(),
        label: "backup".into(),
        service: "backup".into(),
    }
}

#[derive(Serialize)]
struct LitestreamYml {
    dbs: Vec<DbConfig>,
}

#[derive(Serialize)]
struct DbConfig {
    path: String,
    replicas: Vec<ReplicaConfig>,
}

#[derive(Serialize)]
struct ReplicaConfig {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "access-key-id")]
    access_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "secret-access-key")]
    secret_access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention: Option<String>,
}

/// Generate litestream.yml content. Litestream ≥0.5 allows one replica per
/// database: the s3/r2 destination when configured, else the local file replica.
pub fn generate_litestream_yml(s3: &S3Config) -> String {
    let replica = if s3.is_configured() {
        ReplicaConfig {
            url: format!("s3://{}/{}", s3.bucket.trim_end_matches('/'), s3.prefix.trim_matches('/')),
            access_key_id: Some(s3.access_key_id.clone()),
            secret_access_key: Some(s3.secret_access_key.clone()),
            endpoint: Some(s3.endpoint.clone()),
            retention: Some("720h".into()),
        }
    } else {
        ReplicaConfig {
            url: SIDECAR_REPLICA_URL.to_string(),
            access_key_id: None,
            secret_access_key: None,
            endpoint: None,
            retention: Some("720h".into()), // 30 days — the platform DB is small
        }
    };
    let yml = LitestreamYml { dbs: vec![DbConfig { path: SIDECAR_DB_PATH.to_string(), replicas: vec![replica] }] };
    serde_yaml::to_string(&yml).expect("litestream yml serializes")
}

/// The replica's remote identity — everything except credentials.
fn replica_target(s3: &S3Config) -> (String, Option<String>) {
    if s3.is_configured() {
        (format!("s3://{}/{}", s3.bucket.trim_end_matches('/'), s3.prefix.trim_matches('/')), Some(s3.endpoint.clone()))
    } else {
        (SIDECAR_REPLICA_URL.to_string(), None)
    }
}

/// Same destination (endpoint/bucket/prefix) — a credential-only update.
pub fn same_destination(a: &S3Config, b: &S3Config) -> bool {
    replica_target(a) == replica_target(b)
}

/// Read the replica target from an existing litestream.yml.
fn yml_replica_target(content: &str) -> Option<(String, Option<String>)> {
    let yml: serde_yaml::Value = serde_yaml::from_str(content).ok()?;
    let replica = yml.get("dbs")?.as_sequence()?.first()?.get("replicas")?.as_sequence()?.first()?;
    let url = replica.get("url")?.as_str()?.to_string();
    let endpoint = replica.get("endpoint").and_then(|v| v.as_str()).map(str::to_string);
    Some((url, endpoint))
}

/// Write litestream.yml into the data dir (shared with the sidecar). Idempotent —
/// only touches the file when content changes. Also ensures the backups dir exists.
/// Litestream's local staging state is cleared only when the replica TARGET changed
/// (mode/bucket/prefix/endpoint) — credential-only updates resume the same store.
pub fn write_litestream_yml(data_dir: &Path, s3: &S3Config) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir.join("backups"))?;
    let content = generate_litestream_yml(s3);
    let path = data_dir.join("litestream.yml");
    let old = std::fs::read_to_string(&path).ok();
    if old.as_deref() != Some(content.as_str()) {
        let target_changed = old
            .as_deref()
            .and_then(yml_replica_target)
            .map(|old_target| old_target != replica_target(s3))
            .unwrap_or(true);
        std::fs::write(&path, &content)?;
        if target_changed {
            // A replica switch invalidates litestream's local staging state and the
            // old file-replica store — clearing both forces a fresh full snapshot.
            let staging = data_dir.join(".litebin.db-litestream");
            if staging.exists() {
                std::fs::remove_dir_all(&staging)?;
            }
            let local_store = data_dir.join("backups").join("litebin.db");
            if local_store.exists() {
                std::fs::remove_dir_all(&local_store)?;
            }
        }
    }
    Ok(())
}

/// Result of probing an s3/r2 destination before saving its config.
#[derive(Debug, PartialEq, Eq)]
pub enum DestinationProbe {
    /// Credentials work; the prefix already holds backup files.
    Existing,
    /// Credentials work; the prefix is empty (fresh destination).
    Empty,
    /// Probe failed — raw litestream output explains why (bad creds, bucket, network).
    Failed(String),
}

/// Classify a `litestream restore` probe by exit code + output. Conservative:
/// unknown errors are Failed with the raw output rather than a guess.
fn classify_probe(exit_code: i64, output: &str) -> DestinationProbe {
    if exit_code == 0 {
        return DestinationProbe::Existing;
    }
    let lower = output.to_lowercase();
    let empty_markers = ["no database state", "no snapshots", "no restore points", "no ltx files"];
    let auth_markers =
        ["accessdenied", "invalidaccesskeyid", "signaturedoesnotmatch", "unauthorized", "forbidden", "403"];
    if empty_markers.iter().any(|m| lower.contains(m)) {
        return DestinationProbe::Empty;
    }
    if auth_markers.iter().any(|m| lower.contains(m)) {
        return DestinationProbe::Failed("credentials rejected by the destination".into());
    }
    DestinationProbe::Failed(output.trim().to_string())
}

/// Probe an s3/r2 destination from the running sidecar with a throwaway config:
/// validates the credentials and reports whether the prefix already holds files.
pub async fn probe_destination(
    docker: &litebin_common::docker::DockerManager,
    data_dir: &Path,
    s3: &S3Config,
) -> anyhow::Result<DestinationProbe> {
    let sidecar = docker
        .find_container_by_name(SIDECAR_CONTAINER_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("backup sidecar is not running"))?;

    let tmp_config = data_dir.join("preflight-litestream.yml");
    let tmp_db = data_dir.join("preflight-probe.db");
    std::fs::write(&tmp_config, generate_litestream_yml(s3))?;
    let cmd = vec![
        "litestream",
        "restore",
        "-config",
        "/data/preflight-litestream.yml",
        "-o",
        "/data/preflight-probe.db",
        "-force",
        SIDECAR_DB_PATH,
    ];
    let exec = tokio::time::timeout(std::time::Duration::from_secs(60), docker.exec_container(&sidecar, &cmd)).await;
    let _ = std::fs::remove_file(&tmp_config);
    let _ = std::fs::remove_file(&tmp_db);
    let exec = exec.map_err(|_| anyhow::anyhow!("destination probe timed out"))??;
    Ok(classify_probe(exec.exit_code, &exec.output))
}

/// Last destination health probe (kept in memory; None before the first check).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DestinationHealth {
    pub ok: bool,
    /// Last probe time (RFC 3339) — internal, feeds the health dot.
    pub checked_at: String,
    /// First persistent failure (RFC 3339, from sidecar error logs) — the
    /// "failing since" shown to users. None while healthy.
    pub failing_since: Option<String>,
    /// Failure detail (auth error, unreachable, sidecar down).
    pub detail: Option<String>,
}

/// Keeps DestinationHealth current: sidecar ERROR lines mark failures in real
/// time (see the watcher below); a 15-minute probe catches quiet breakage.
pub fn spawn_destination_probes(
    db: sqlx::SqlitePool,
    config: std::sync::Arc<crate::config::Config>,
    docker: std::sync::Arc<litebin_common::docker::DockerManager>,
    health: std::sync::Arc<std::sync::RwLock<Option<DestinationHealth>>>,
) {
    tokio::spawn(async move {
        // Sidecar ERROR lines mark the destination failing (recording when it
        // started) and trigger an immediate probe; litestream's periodic success
        // lines (retention/compaction, every ~15-30s) clear it on recovery. Both
        // debounced to once a minute.
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        {
            let docker = docker.clone();
            let notify = notify.clone();
            let health = health.clone();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            docker.spawn_log_watcher(SIDECAR_CONTAINER_NAME.to_string(), tx);
            tokio::spawn(async move {
                let mut last_error = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(120))
                    .unwrap_or_else(std::time::Instant::now);
                let mut last_recover = last_error;
                while let Some(line) = rx.recv().await {
                    if line.contains("level=ERROR") {
                        if last_error.elapsed() >= std::time::Duration::from_secs(60) {
                            last_error = std::time::Instant::now();
                            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                            let mut guard = health.write().expect("destination health lock");
                            let failing_since = guard.as_ref().and_then(|h| h.failing_since.clone()).unwrap_or(now);
                            *guard = Some(DestinationHealth {
                                ok: false,
                                checked_at: guard.as_ref().map(|h| h.checked_at.clone()).unwrap_or_default(),
                                failing_since: Some(failing_since),
                                detail: Some("litestream reported errors".into()),
                            });
                            drop(guard);
                            notify.notify_one();
                        }
                    } else if (line.contains("l0 retention enforced") || line.contains("compaction complete"))
                        && last_recover.elapsed() >= std::time::Duration::from_secs(60)
                    {
                        last_recover = std::time::Instant::now();
                        let mut guard = health.write().expect("destination health lock");
                        if guard.as_ref().is_some_and(|h| !h.ok) {
                            *guard = Some(DestinationHealth {
                                ok: true,
                                checked_at: guard.as_ref().map(|h| h.checked_at.clone()).unwrap_or_default(),
                                failing_since: None,
                                detail: None,
                            });
                            tracing::info!("backup: destination recovered (sidecar reports healthy operations)");
                        }
                    }
                }
            });
        }
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        loop {
            let enabled = get_setting(&db, SETTING_BACKUP_ENABLED).await.ok().flatten().as_deref() == Some("1");
            let s3 = S3Config {
                endpoint: setting_or_default(&db, SETTING_S3_ENDPOINT).await,
                bucket: setting_or_default(&db, SETTING_S3_BUCKET).await,
                prefix: setting_or_default(&db, SETTING_S3_PREFIX).await,
                access_key_id: setting_or_default(&db, SETTING_S3_ACCESS_KEY).await,
                secret_access_key: setting_or_default(&db, SETTING_S3_SECRET_KEY).await,
            };
            if !enabled || !s3.is_configured() {
                *health.write().expect("destination health lock") = None;
                tokio::time::sleep(std::time::Duration::from_secs(900)).await;
                continue;
            }
            let checked_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let (ok, detail) = match resolve_data_dir(&config.database_url) {
                Some(data_dir) => match probe_destination(&docker, &data_dir, &s3).await {
                    Ok(DestinationProbe::Existing) | Ok(DestinationProbe::Empty) => (true, None),
                    Ok(DestinationProbe::Failed(d)) => (false, Some(d)),
                    Err(e) => (false, Some(e.to_string())),
                },
                None => (false, Some("in-memory database".to_string())),
            };
            tracing::debug!(ok, detail = ?detail, "backup: destination health probe");
            if !ok {
                tracing::warn!(detail = ?detail, "backup: destination health probe failed");
            }
            let failing_since = {
                let guard = health.read().expect("destination health lock");
                if ok { None } else { guard.as_ref().and_then(|h| h.failing_since.clone()) }
            };
            *health.write().expect("destination health lock") =
                Some(DestinationHealth { ok, checked_at, failing_since, detail });
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(900)) => {}
                _ = notify.notified() => {}
            }
        }
    });
}

/// Apply a pending restore produced by `litestream restore -o litebin.db.restored`.
/// Must run before the DB pool is opened. The previous db (plus its -wal/-shm) is
/// kept beside it as `litebin.db.pre-restore*` — a one-shot safety copy that the
/// next restore overwrites. Returns true when a swap happened.
pub fn apply_pending_restore(data_dir: &Path) -> bool {
    let restored = data_dir.join("litebin.db.restored");
    if !restored.exists() {
        return false;
    }

    let db = data_dir.join("litebin.db");
    let pre = data_dir.join("litebin.db.pre-restore");

    if db.exists() {
        for (from, to) in [
            (data_dir.join("litebin.db"), pre.clone()),
            (data_dir.join("litebin.db-wal"), data_dir.join("litebin.db.pre-restore-wal")),
            (data_dir.join("litebin.db-shm"), data_dir.join("litebin.db.pre-restore-shm")),
        ] {
            let _ = std::fs::remove_file(&to);
            if from.exists()
                && let Err(e) = std::fs::rename(&from, &to)
            {
                tracing::error!(error = %e, "restore: could not move {} aside", from.display());
                let _ = std::fs::remove_file(&restored);
                return false;
            }
        }
    }

    // The restored file is a plain snapshot — no WAL applies to it, unless the
    // restore staged one (revert), in which case it becomes the live WAL.
    match std::fs::rename(&restored, &db) {
        Ok(()) => {
            let restored_wal = data_dir.join("litebin.db.restored-wal");
            if restored_wal.exists() {
                let _ = std::fs::rename(&restored_wal, data_dir.join("litebin.db-wal"));
            }
            tracing::info!(
                pre_restore = %pre.display(),
                "applied pending database restore from backup"
            );
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "restore: could not move restored db into place");
            let _ = std::fs::remove_file(&restored);
            false
        }
    }
}

/// Recursively collect (file_count, size, newest mtime) under a directory.
/// Litestream ≥0.5 nests replica files as `<generation>/<level>/<txid>.ltx`,
/// so a bounded recursive walk is required.
fn walk_files(dir: &Path, depth: u8) -> (u64, u64, Option<SystemTime>) {
    let (mut count, mut size, mut newest) = (0u64, 0u64, None);
    if depth == 0 {
        return (count, size, newest);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (count, size, newest);
    };
    for entry in entries.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        if md.is_dir() {
            let (c, s, t) = walk_files(&entry.path(), depth - 1);
            count += c;
            size += s;
            newest = newest.max(t);
        } else {
            count += 1;
            size += md.len();
            if let Ok(t) = md.modified() {
                newest = newest.max(Some(t));
            }
        }
    }
    (count, size, newest)
}

/// Local replica retention configured in litestream.yml (`retention: 720h`).
pub const REPLICA_RETENTION_SECS: u64 = 720 * 3600;

/// Snapshot of the local LTX store. Litestream ≥0.5 keeps one continuous store
/// at `<replica>/ltx/<level>/<minTXID>-<maxTXID>.ltx` — no generation dirs — so
/// restore coverage is a window: [coverage_start, now].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ReplicaInfo {
    /// Earliest restorable moment (RFC 3339): the later of store creation and
    /// the retention horizon.
    pub coverage_start: Option<String>,
    pub file_count: u64,
    pub size_bytes: u64,
    /// Newest LTX write (RFC 3339) — None before the first sync.
    pub last_write: Option<String>,
}

pub(crate) fn system_time_to_rfc3339(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    chrono::DateTime::from_timestamp(secs as i64, 0).map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Summarize the local replica. Returns None when the store doesn't exist yet.
pub fn replica_info(replica_dir: &Path) -> Option<ReplicaInfo> {
    let md = std::fs::metadata(replica_dir).ok()?;
    let (file_count, size_bytes, newest) = walk_files(replica_dir, 4);
    // The store dir is created once (its only child `ltx` is added at init), so its
    // mtime is a stable "backup began" marker. Retention bounds how far back data goes.
    let store_created = md.modified().ok();
    let retention_horizon = SystemTime::now().checked_sub(std::time::Duration::from_secs(REPLICA_RETENTION_SECS));
    let coverage_start = store_created
        .zip(retention_horizon)
        .map(|(created, horizon)| created.max(horizon))
        .or(store_created)
        .and_then(system_time_to_rfc3339);
    Some(ReplicaInfo { coverage_start, file_count, size_bytes, last_write: newest.and_then(system_time_to_rfc3339) })
}

/// Local daily snapshots retained alongside the s3/r2 destination.
const DAILY_RETENTION_DAYS: u64 = 30;

/// A dated local snapshot restored from the s3/r2 replica.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DailySnapshot {
    /// File-name date, UTC (YYYY-MM-DD).
    pub date: String,
    /// When the snapshot was taken (RFC 3339, file mtime).
    pub timestamp: String,
    pub size_bytes: u64,
}

/// List local daily snapshots, newest first.
pub fn daily_snapshots(data_dir: &Path) -> Vec<DailySnapshot> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(data_dir.join("backups").join("daily")) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(date) = name.strip_prefix("litebin-").and_then(|n| n.strip_suffix(".db")) else { continue };
        let Ok(md) = entry.metadata() else { continue };
        let timestamp = md.modified().ok().and_then(system_time_to_rfc3339).unwrap_or_default();
        out.push(DailySnapshot { date: date.to_string(), timestamp, size_bytes: md.len() });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// Snapshot file backing a named restore point.
pub fn point_file(data_dir: &Path, id: i64) -> PathBuf {
    data_dir.join("backups").join("points").join(format!("point-{id}.db"))
}

/// Take a named restore point: a `VACUUM INTO` snapshot of the live DB kept as a
/// local file, so points survive replica switches and disabled backup. Returns
/// (id, RFC 3339 timestamp, size).
pub async fn take_named_point(
    db: &sqlx::SqlitePool,
    data_dir: &Path,
    name: &str,
) -> anyhow::Result<(i64, String, u64)> {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let id: i64 = sqlx::query("INSERT INTO backup_points (name, timestamp) VALUES (?, ?)")
        .bind(name)
        .bind(&timestamp)
        .execute(db)
        .await?
        .last_insert_rowid();
    let file = point_file(data_dir, id);
    std::fs::create_dir_all(file.parent().expect("point dir has a parent"))?;
    if let Err(e) = sqlx::query("VACUUM INTO ?").bind(file.to_string_lossy().to_string()).execute(db).await {
        let _ = sqlx::query("DELETE FROM backup_points WHERE id = ?").bind(id).execute(db).await;
        anyhow::bail!("point snapshot failed: {e}");
    }
    let size_bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    tracing::info!(point = id, name = %name, size_bytes, "backup: named restore point saved");
    Ok((id, timestamp, size_bytes))
}

/// Snapshot the current s3/r2 replica state into a dated local file — the
/// just-in-case copy, and daily proof the remote replica is restorable.
pub async fn take_daily_snapshot(
    docker: &litebin_common::docker::DockerManager,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let sidecar = docker
        .find_container_by_name(SIDECAR_CONTAINER_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("backup sidecar not found"))?;
    let daily_dir = data_dir.join("backups").join("daily");
    std::fs::create_dir_all(&daily_dir)?;
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let out = format!("/data/backups/daily/litebin-{date}.db");
    let cmd = vec!["litestream", "restore", "-config", SIDECAR_CONFIG_PATH, "-o", &out, "-force", SIDECAR_DB_PATH];
    let exec = tokio::time::timeout(std::time::Duration::from_secs(300), docker.exec_container(&sidecar, &cmd))
        .await
        .map_err(|_| anyhow::anyhow!("daily snapshot timed out"))??;
    if exec.exit_code != 0 {
        anyhow::bail!("daily snapshot failed: {}", exec.output);
    }
    enforce_daily_retention(&daily_dir);
    tracing::info!(date = %date, "backup: local daily snapshot restored from s3/r2");
    Ok(())
}

fn enforce_daily_retention(daily_dir: &Path) {
    let cutoff = SystemTime::now().checked_sub(std::time::Duration::from_secs(DAILY_RETENTION_DAYS * 86_400));
    let Ok(entries) = std::fs::read_dir(daily_dir) else { return };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        if md.is_file() {
            files.push((md.modified().ok(), entry.path()));
        }
    }
    let newest = files
        .iter()
        .max_by_key(|(t, _)| t.map(|t| t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default()))
        .map(|(_, p)| p.clone());
    for (mtime, path) in files {
        if newest.as_deref() == Some(path.as_path()) {
            continue;
        }
        if mtime.zip(cutoff).map(|(t, c)| t < c).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Background loop: daily local snapshot while backup is enabled and an s3/r2
/// destination is configured (retries on failure; re-checks settings hourly).
pub fn spawn_daily_snapshots(
    db: sqlx::SqlitePool,
    config: std::sync::Arc<crate::config::Config>,
    docker: std::sync::Arc<litebin_common::docker::DockerManager>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        loop {
            let enabled = get_setting(&db, SETTING_BACKUP_ENABLED).await.ok().flatten().as_deref() == Some("1");
            let s3_configured = !setting_or_default(&db, SETTING_S3_ENDPOINT).await.is_empty()
                && !setting_or_default(&db, SETTING_S3_BUCKET).await.is_empty();
            let Some(data_dir) = resolve_data_dir(&config.database_url) else {
                return;
            };
            if !enabled || !s3_configured {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                continue;
            }
            match take_daily_snapshot(&docker, &data_dir).await {
                Ok(()) => tokio::time::sleep(std::time::Duration::from_secs(86_400)).await,
                Err(e) => {
                    tracing::warn!(error = %e, "backup: daily local snapshot failed — retrying in 10m");
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                }
            }
        }
    });
}

async fn setting_or_default(db: &sqlx::SqlitePool, key: &str) -> String {
    get_setting(db, key).await.ok().flatten().unwrap_or_default()
}

/// Boot-time reconciliation: refresh litestream.yml from settings, and if backup is
/// enabled, make sure the sidecar container exists and is running.
pub async fn reconcile(
    db: &sqlx::SqlitePool,
    config: &crate::config::Config,
    docker: &litebin_common::docker::DockerManager,
) {
    let Some(data_dir) = resolve_data_dir(&config.database_url) else {
        return;
    };

    let s3 = S3Config {
        endpoint: setting_or_default(db, SETTING_S3_ENDPOINT).await,
        bucket: setting_or_default(db, SETTING_S3_BUCKET).await,
        prefix: setting_or_default(db, SETTING_S3_PREFIX).await,
        access_key_id: setting_or_default(db, SETTING_S3_ACCESS_KEY).await,
        secret_access_key: setting_or_default(db, SETTING_S3_SECRET_KEY).await,
    };
    if let Err(e) = write_litestream_yml(&data_dir, &s3) {
        tracing::warn!(error = %e, "backup: failed to write litestream.yml");
    }

    if get_setting(db, SETTING_BACKUP_ENABLED).await.ok().flatten().as_deref() != Some("1") {
        return;
    }
    let Some(bind) = docker.detect_mount_bind_source(&data_dir.to_string_lossy()).await else {
        tracing::warn!("backup enabled but no shared data mount detected — sidecar not started");
        return;
    };
    if let Err(e) = docker.ensure_sidecar_container(&sidecar_spec(docker.network(), &bind)).await {
        tracing::warn!(error = %e, "backup: failed to ensure sidecar container");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn named_point_is_a_valid_snapshot() {
        let dir = std::env::temp_dir().join(format!("l8b-point-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(dir.join("t.db")).create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new().connect_with(opts).await.unwrap();
        sqlx::query("CREATE TABLE backup_points (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, timestamp TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now')))").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE t(x INTEGER)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO t VALUES (42)").execute(&pool).await.unwrap();

        let (id, _ts, size) = take_named_point(&pool, &dir, "before-migration").await.unwrap();
        assert!(size > 0);
        assert!(point_file(&dir, id).is_file());

        // The snapshot must open as a database containing the live data.
        let snap_opts = sqlx::sqlite::SqliteConnectOptions::new().filename(point_file(&dir, id));
        let snap = sqlx::sqlite::SqlitePoolOptions::new().connect_with(snap_opts).await.unwrap();
        let x: i64 = sqlx::query_scalar("SELECT x FROM t").fetch_one(&snap).await.unwrap();
        assert_eq!(x, 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn credential_rotation_keeps_staging() {
        let dir = std::env::temp_dir().join(format!("l8b-rot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s3 = |key: &str| S3Config {
            endpoint: "https://acct.r2.cloudflarestorage.com".into(),
            bucket: "buck".into(),
            prefix: "p".into(),
            access_key_id: key.into(),
            secret_access_key: "s".into(),
        };
        write_litestream_yml(&dir, &s3("AKIA1")).unwrap();
        std::fs::create_dir_all(dir.join(".litebin.db-litestream")).unwrap();

        write_litestream_yml(&dir, &s3("AKIA2")).unwrap();
        assert!(dir.join(".litebin.db-litestream").exists(), "rotation must not clear staging");

        let mut moved = s3("AKIA2");
        moved.bucket = "other".into();
        write_litestream_yml(&dir, &moved).unwrap();
        assert!(!dir.join(".litebin.db-litestream").exists(), "target change must clear staging");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_classification() {
        assert_eq!(classify_probe(0, ""), DestinationProbe::Existing);
        assert_eq!(classify_probe(1, "Error: no database state found"), DestinationProbe::Empty);
        assert_eq!(classify_probe(1, "no snapshots found for path"), DestinationProbe::Empty);
        assert_eq!(
            classify_probe(1, "AccessDenied: ..."),
            DestinationProbe::Failed("credentials rejected by the destination".into())
        );
        assert_eq!(classify_probe(1, "NoSuchBucket: buck"), DestinationProbe::Failed("NoSuchBucket: buck".into()));
    }

    #[test]
    fn yml_local_only() {
        let yml = generate_litestream_yml(&S3Config::default());
        assert!(yml.contains("path: /data/litebin.db"));
        assert!(yml.contains("url: file:///data/backups/litebin.db"));
        assert!(!yml.contains("s3://"));
    }

    #[test]
    fn yml_with_s3() {
        let yml = generate_litestream_yml(&S3Config {
            endpoint: "https://acct.r2.cloudflarestorage.com".into(),
            bucket: "buck".into(),
            prefix: "litebin/".into(),
            access_key_id: "AKIA".into(),
            secret_access_key: "s3cr3t".into(),
        });
        assert!(yml.contains("url: s3://buck/litebin"));
        assert!(yml.contains("endpoint: https://acct.r2.cloudflarestorage.com"));
        assert!(yml.contains("access-key-id: AKIA"));
        assert!(yml.contains("secret-access-key: s3cr3t"));
        assert!(!yml.contains("file://"));
    }

    #[test]
    fn resolves_data_dir() {
        let relative = resolve_data_dir("sqlite:./data/litebin.db").unwrap();
        assert!(relative.is_absolute());
        assert!(relative.ends_with("data"));
        #[cfg(unix)]
        assert_eq!(resolve_data_dir("sqlite:/app/data/litebin.db"), Some(PathBuf::from("/app/data")));
        assert_eq!(resolve_data_dir("sqlite::memory:"), None);
    }

    #[test]
    fn restore_swap_replaces_db_and_clears_wal() {
        let dir = std::env::temp_dir().join(format!("l8b-restore-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("litebin.db"), b"old").unwrap();
        std::fs::write(dir.join("litebin.db-wal"), b"stale-wal").unwrap();
        std::fs::write(dir.join("litebin.db.restored"), b"new").unwrap();

        apply_pending_restore(&dir);

        assert_eq!(std::fs::read(dir.join("litebin.db")).unwrap(), b"new");
        assert!(!dir.join("litebin.db-wal").exists());
        assert!(!dir.join("litebin.db.restored").exists());
        assert_eq!(std::fs::read(dir.join("litebin.db.pre-restore")).unwrap(), b"old");
        assert_eq!(std::fs::read(dir.join("litebin.db.pre-restore-wal")).unwrap(), b"stale-wal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_swap_noop_without_pending_file() {
        let dir = std::env::temp_dir().join(format!("l8b-restore-noop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("litebin.db"), b"untouched").unwrap();

        apply_pending_restore(&dir);

        assert_eq!(std::fs::read(dir.join("litebin.db")).unwrap(), b"untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replica_info_walks_ltx_store() {
        let dir = std::env::temp_dir().join(format!("l8b-replica-info-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Litestream ≥0.5 layout: <replica>/ltx/<level>/<minTXID>-<maxTXID>.ltx
        let level = dir.join("ltx/0");
        std::fs::create_dir_all(&level).unwrap();
        for i in 0..2 {
            std::fs::write(level.join(format!("{i:016}-{i:016}.ltx")), vec![0u8; 100]).unwrap();
        }

        let info = replica_info(&dir).expect("store exists");
        assert_eq!(info.file_count, 2);
        assert_eq!(info.size_bytes, 200);
        assert!(info.coverage_start.is_some(), "coverage starts at store creation");
        assert!(info.last_write.is_some());
        assert!(replica_info(dir.join("nonexistent").as_path()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replica_info_coverage_never_precedes_retention_horizon() {
        let dir = std::env::temp_dir().join(format!("l8b-retention-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ltx/9")).unwrap();
        std::fs::write(dir.join("ltx/9/0000000000000001-0000000000000002.ltx"), b"x").unwrap();

        let info = replica_info(&dir).expect("store exists");
        // A fresh store's coverage starts ~now (well after the retention horizon).
        let coverage = info.coverage_start.unwrap();
        let horizon = chrono::Utc::now() - chrono::Duration::seconds(REPLICA_RETENTION_SECS as i64 + 60);
        assert!(coverage.parse::<chrono::DateTime<chrono::Utc>>().unwrap() > horizon);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
