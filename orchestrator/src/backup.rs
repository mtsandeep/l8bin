//! Litestream backup sidecar: config generation, restore swap, and replica status.
//!
//! Backup is opt-in (dashboard/CLI `enable`), master-only. The orchestrator owns
//! `/app/data/litestream.yml` and manages the `litebin-litestream` sidecar container
//! directly via the Docker API — no compose involvement.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::routes::global_settings::get_setting;

pub const SIDECAR_CONTAINER_NAME: &str = "litebin-litestream";
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

/// Generate litestream.yml content: local file replica always, S3 replica when configured.
pub fn generate_litestream_yml(s3: &S3Config) -> String {
    let mut replicas = vec![ReplicaConfig {
        url: SIDECAR_REPLICA_URL.to_string(),
        access_key_id: None,
        secret_access_key: None,
        endpoint: None,
        retention: Some("720h".into()), // 30 days — the platform DB is small
    }];
    if s3.is_configured() {
        replicas.push(ReplicaConfig {
            url: format!("s3://{}/{}", s3.bucket.trim_end_matches('/'), s3.prefix.trim_matches('/')),
            access_key_id: Some(s3.access_key_id.clone()),
            secret_access_key: Some(s3.secret_access_key.clone()),
            endpoint: Some(s3.endpoint.clone()),
            retention: Some("720h".into()),
        });
    }
    let yml = LitestreamYml { dbs: vec![DbConfig { path: SIDECAR_DB_PATH.to_string(), replicas }] };
    serde_yaml::to_string(&yml).expect("litestream yml serializes")
}

/// Write litestream.yml into the data dir (shared with the sidecar). Idempotent —
/// only touches the file when content changes. Also ensures the backups dir exists.
pub fn write_litestream_yml(data_dir: &Path, s3: &S3Config) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir.join("backups"))?;
    let content = generate_litestream_yml(s3);
    let path = data_dir.join("litestream.yml");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        std::fs::write(&path, &content)?;
    }
    Ok(())
}

/// Apply a pending restore produced by `litestream restore -o litebin.db.restored`.
/// Must run before the DB pool is opened. The previous db (plus its -wal/-shm) is
/// kept beside it as `litebin.db.pre-restore*` — a one-shot safety copy that the
/// next restore overwrites.
pub fn apply_pending_restore(data_dir: &Path) {
    let restored = data_dir.join("litebin.db.restored");
    if !restored.exists() {
        return;
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
                return;
            }
        }
    }

    // The restored file is a plain snapshot — no WAL applies to it.
    match std::fs::rename(&restored, &db) {
        Ok(()) => tracing::info!(
            pre_restore = %pre.display(),
            "applied pending database restore from backup"
        ),
        Err(e) => {
            tracing::error!(error = %e, "restore: could not move restored db into place");
            let _ = std::fs::remove_file(&restored);
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

fn system_time_to_rfc3339(t: SystemTime) -> Option<String> {
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
