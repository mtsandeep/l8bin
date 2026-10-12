//! `l8b backup` — platform SQLite backup via the Litestream sidecar (admin scope).

use anyhow::{Result, bail};
use serde::Serialize;

use crate::auth;
use crate::ci::CiMode;
use crate::config;
use crate::out::{self, Out};

/// Map 403s (embedded in api_* error text) to an admin-pairing hint.
fn map_admin_err(e: anyhow::Error) -> anyhow::Error {
    let msg = format!("{e:#}");
    if msg.contains("(403") {
        out::fail(msg, "admin scope required: l8b login --server <url> --pair (pick admin when approving)")
    } else {
        e
    }
}

fn target(server_flag: Option<&str>, token_flag: Option<&str>) -> Result<(reqwest::Client, String)> {
    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    Ok((target.client, target.server))
}

fn print_status(status: &serde_json::Value) {
    let enabled = status["enabled"].as_bool().unwrap_or(false);
    let running = status["sidecar_running"].as_bool().unwrap_or(false);
    let last_sync = status["last_sync"].as_str().unwrap_or("never");
    if !enabled {
        println!("backup:      disabled");
        return;
    }
    println!("backup:      {} (last sync: {last_sync})", if running { "running" } else { "sidecar not running" });
    let s3 = &status["s3"];
    if s3["configured"].as_bool().unwrap_or(false) {
        println!(
            "destination: s3://{}{} ({})",
            s3["bucket"].as_str().unwrap_or_default(),
            s3["prefix"].as_str().map(|p| format!("/{p}")).unwrap_or_default(),
            s3["endpoint"].as_str().unwrap_or_default()
        );
        if let Some(h) = status["destination_health"].as_object()
            && h["ok"].as_bool() == Some(false)
        {
            let since = h["failing_since"].as_str().unwrap_or("?");
            println!("destination health: ERROR — {} (failing since {since})", h["detail"].as_str().unwrap_or("?"),);
        }
        if let Some(daily) = status["dailies"].as_array().and_then(|d| d.first()) {
            println!(
                "local daily: {} ({})",
                daily["date"].as_str().unwrap_or("?"),
                daily["timestamp"].as_str().unwrap_or("?")
            );
        } else {
            println!("local daily: none yet");
        }
    } else {
        println!("destination: local (this server)");
    }
    if let Some(replica) = status["replica"].as_object() {
        let coverage = replica["coverage_start"].as_str().unwrap_or("—");
        let files = replica["file_count"].as_u64().unwrap_or(0);
        let bytes = replica["size_bytes"].as_u64().unwrap_or(0);
        println!("replica:     {files} files, {bytes} bytes (coverage from {coverage})");
    }
    if let Some(db_bytes) = status["db_size_bytes"].as_u64() {
        println!("live db:     {db_bytes} bytes");
    }
    if let Some(to) = status["revertible_to"].as_str() {
        println!("undo:        available (to {to})");
    }
    if let Some(points) = status["points"].as_array()
        && !points.is_empty()
    {
        println!("manual backups:");
        for pt in points {
            let size = if pt["available"].as_bool().unwrap_or(false) {
                format!("{} bytes", pt["size_bytes"].as_u64().unwrap_or(0))
            } else {
                "snapshot missing".into()
            };
            println!(
                "  #{:<3} {:<24} {} ({})",
                pt["id"].as_i64().unwrap_or(0),
                pt["name"].as_str().unwrap_or("?"),
                pt["timestamp"].as_str().unwrap_or("?"),
                size
            );
        }
    }
}

/// `l8b backup status`
pub(crate) async fn status(server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let (client, server) = target(server_flag, token_flag)?;
    let v = auth::api_get(&client, &server, "/backup/status").await.map_err(map_admin_err)?;
    out.ok(&v);
    if !out.json {
        print_status(&v);
    }
    Ok(())
}

/// `l8b backup enable`
pub(crate) async fn enable(server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let (client, server) = target(server_flag, token_flag)?;
    out.note("Enabling backup (pulls the litestream image on first run — may take ~30s)…");
    let v =
        auth::api_post_json(&client, &server, "/backup/enable", &serde_json::json!({})).await.map_err(map_admin_err)?;
    out.ok(&v);
    if !out.json {
        print_status(&v["status"]);
    }
    Ok(())
}

fn confirm_or_yes(label: &str, yes: bool, ci_mode: &CiMode, out: &Out) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if out.json || ci_mode.enabled {
        bail!(out::fail(format!("{label} needs confirmation"), "re-run with --yes"));
    }
    Ok(dialoguer::Confirm::new().with_prompt(label).default(false).interact().unwrap_or(false))
}

/// `l8b backup disable`
pub(crate) async fn disable(
    yes: bool,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &CiMode,
    out: &Out,
) -> Result<()> {
    if !confirm_or_yes("Disable backup? Existing replicas are kept", yes, ci_mode, out)? {
        out.note("Aborted.");
        return Ok(());
    }
    let (client, server) = target(server_flag, token_flag)?;
    let v = auth::api_post_json(&client, &server, "/backup/disable", &serde_json::json!({}))
        .await
        .map_err(map_admin_err)?;
    out.ok(&v);
    if !out.json {
        println!("Backup disabled.");
    }
    Ok(())
}

pub(crate) struct AddS3Args {
    pub endpoint: String,
    pub bucket: String,
    pub prefix: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub yes: bool,
}

#[derive(Serialize)]
struct AddS3Result {
    s3_configured: bool,
    last_sync: Option<String>,
}

/// `l8b backup add-s3`
pub(crate) async fn add_s3(
    args: AddS3Args,
    ci_mode: &CiMode,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    ci_mode.mask_secret(&args.secret_key);
    let (client, server) = target(server_flag, token_flag)?;
    let body = serde_json::json!({
        "endpoint": args.endpoint.trim(),
        "bucket": args.bucket.trim(),
        "prefix": args.prefix.as_deref().map(str::trim).filter(|p| !p.is_empty()),
        "access_key_id": args.access_key.trim(),
        "secret_access_key": args.secret_key.trim(),
    });
    let mut v = auth::api_post_json(&client, &server, "/backup/s3", &body).await.map_err(map_admin_err)?;
    if v["existing_files"].as_bool().unwrap_or(false) {
        let label = "Destination already holds backup files; start a new backup alongside them?";
        if !args.yes && !confirm_or_yes(label, false, ci_mode, out)? {
            out.note("Aborted.");
            return Ok(());
        }
        v = auth::api_post_json(
            &client,
            &server,
            "/backup/s3",
            &serde_json::json!({
                "endpoint": args.endpoint.trim(),
                "bucket": args.bucket.trim(),
                "prefix": args.prefix.as_deref().map(str::trim).filter(|p| !p.is_empty()),
                "access_key_id": args.access_key.trim(),
                "secret_access_key": args.secret_key.trim(),
                "confirm_existing": true,
            }),
        )
        .await
        .map_err(map_admin_err)?;
    }
    let result = AddS3Result {
        s3_configured: v["status"]["s3"]["configured"].as_bool().unwrap_or(false),
        last_sync: v["status"]["last_sync"].as_str().map(str::to_string),
    };
    out.ok(&result);
    if !out.json {
        println!("Backup destination saved — litestream restarted.");
    }
    Ok(())
}

/// `l8b backup remove-s3`
pub(crate) async fn remove_s3(
    yes: bool,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &CiMode,
    out: &Out,
) -> Result<()> {
    if !confirm_or_yes("Remove the S3 replica? Local backup keeps running", yes, ci_mode, out)? {
        out.note("Aborted.");
        return Ok(());
    }
    let (client, server) = target(server_flag, token_flag)?;
    let v = auth::api_delete(&client, &server, "/backup/s3").await.map_err(map_admin_err)?;
    out.ok(&v);
    if !out.json {
        println!("S3 replica removed.");
    }
    Ok(())
}

pub(crate) struct RestoreArgs {
    pub timestamp: Option<String>,
    pub from_local: Option<String>,
    pub point: Option<i64>,
    pub yes: bool,
}

/// `l8b backup restore` — stages the restore; the orchestrator restarts to apply it.
pub(crate) async fn restore(
    args: RestoreArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &CiMode,
    out: &Out,
) -> Result<()> {
    let (client, server) = target(server_flag, token_flag)?;

    if let Some(point_id) = args.point {
        let label = format!("Restore saved point {point_id}? Everything written since it was taken is discarded");
        if !confirm_or_yes(&label, args.yes, ci_mode, out)? {
            out.note("Aborted.");
            return Ok(());
        }
        auth::api_post_json(&client, &server, "/backup/restore-point", &serde_json::json!({ "point_id": point_id }))
            .await
            .map_err(map_admin_err)?;
        #[derive(Serialize)]
        struct PointRestoreResult {
            restarting: bool,
            point_id: i64,
        }
        out.ok(&PointRestoreResult { restarting: true, point_id });
        if !out.json {
            println!("Point restore staged — the orchestrator is restarting to apply it (a few seconds).");
        }
        return Ok(());
    }

    if let Some(date) = args.from_local.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        let label =
            format!("Restore the local daily snapshot from {date}? (bypasses R2/S3; everything since is discarded)");
        if !confirm_or_yes(&label, args.yes, ci_mode, out)? {
            out.note("Aborted.");
            return Ok(());
        }
        auth::api_post_json(&client, &server, "/backup/restore-local", &serde_json::json!({ "date": date }))
            .await
            .map_err(map_admin_err)?;
        #[derive(Serialize)]
        struct LocalRestoreResult {
            restarting: bool,
            date: String,
        }
        out.ok(&LocalRestoreResult { restarting: true, date: date.to_string() });
        if !out.json {
            println!("Local daily restore staged — the orchestrator is restarting to apply it (a few seconds).");
        }
        return Ok(());
    }

    let when = args.timestamp.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let label = match when {
        Some(t) => format!("Restore the database to {t}? Everything written since is discarded"),
        None => "Restore the database to its latest backup? Everything written since is discarded".to_string(),
    };
    if !confirm_or_yes(&label, args.yes, ci_mode, out)? {
        out.note("Aborted.");
        return Ok(());
    }

    auth::api_post_json(&client, &server, "/backup/restore", &serde_json::json!({ "timestamp": when }))
        .await
        .map_err(map_admin_err)?;

    #[derive(Serialize)]
    struct RestoreResult {
        restarting: bool,
        timestamp: Option<String>,
    }
    let result = RestoreResult { restarting: true, timestamp: when.map(str::to_string) };
    out.ok(&result);
    if !out.json {
        println!("Restore staged — the orchestrator is restarting to apply it (a few seconds).");
        println!("The previous database is kept as litebin.db.pre-restore on the server.");
    }
    Ok(())
}

/// `l8b backup revert` — undo the last applied restore.
pub(crate) async fn revert(
    yes: bool,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &CiMode,
    out: &Out,
) -> Result<()> {
    if !confirm_or_yes(
        "Undo the last restore? The database returns to the state right before it (everything since is discarded)",
        yes,
        ci_mode,
        out,
    )? {
        out.note("Aborted.");
        return Ok(());
    }
    let (client, server) = target(server_flag, token_flag)?;
    auth::api_post_json(&client, &server, "/backup/revert-restore", &serde_json::json!({}))
        .await
        .map_err(map_admin_err)?;

    #[derive(Serialize)]
    struct RevertResult {
        restarting: bool,
    }
    out.ok(&RevertResult { restarting: true });
    if !out.json {
        println!("Undo staged — the orchestrator is restarting to apply it (a few seconds).");
    }
    Ok(())
}
