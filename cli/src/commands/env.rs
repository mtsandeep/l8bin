//! `l8b env` — runtime env management. Values are write-only: pushed from a
//! file or stdin (never argv), returned only as masked previews.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde_json::json;

use serde::Serialize;

use crate::auth;
use crate::config;
use crate::out::Out;
use crate::status;

#[derive(Serialize)]
struct EnvPayload {
    project_id: String,
    vars: serde_json::Value,
    pending_apply: bool,
}

pub(crate) struct EnvListArgs {
    pub project: String,
}

pub(crate) struct EnvPushArgs {
    pub project: String,
    /// Read vars from this file (default `.env`).
    pub file: Option<std::path::PathBuf>,
    /// Read vars from stdin instead of a file.
    pub stdin: bool,
    /// Replace the whole env file instead of merging.
    pub replace: bool,
    /// Recreate the container after a successful push.
    pub apply: bool,
}

pub(crate) async fn list(
    args: EnvListArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    let client = target.client;
    let server = target.server;

    let env = auth::api_get(&client, &server, &format!("/projects/{}/env", args.project)).await?;
    out.ok(&EnvPayload {
        project_id: args.project.clone(),
        vars: env["vars"].clone(),
        pending_apply: env["pending_apply"].as_bool().unwrap_or(false),
    });
    if !out.json {
        print_env(&args.project, &env);
    }
    Ok(())
}

pub(crate) async fn push(
    args: EnvPushArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &crate::ci::CiMode,
    out: &Out,
) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    let client = target.client;
    let server = target.server;

    let raw: Vec<u8> = if args.stdin {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).context("failed to read env from stdin")?;
        buf
    } else {
        let path = args.file.clone().unwrap_or_else(|| std::path::PathBuf::from(".env"));
        if !path.exists() {
            bail!("env file '{}' not found. Use --file <path> or --stdin.", path.display());
        }
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?
    };

    let parsed: HashMap<String, String> = dotenvy::Iter::new(raw.as_slice())
        .filter_map(|item| item.ok())
        .map(|(k, v)| (k.trim().to_string(), v))
        .collect();
    if parsed.is_empty() {
        bail!("no environment variables found in input");
    }

    let mode = if args.replace { "replace" } else { "merge" };
    let count = parsed.len();
    let body = json!({"env": parsed, "mode": mode});
    let resp = auth::api_put_json(&client, &server, &format!("/projects/{}/env", args.project), &body).await?;

    out.ok(&EnvPayload {
        project_id: args.project.clone(),
        vars: resp["vars"].clone(),
        pending_apply: resp["pending_apply"].as_bool().unwrap_or(false),
    });

    if ci_mode.enabled {
        println!("Updated {count} variable(s) (mode: {mode}).");
    } else {
        println!("{} Updated {count} variable(s) (mode: {mode}).", "✓".green());
        print_env(&args.project, &resp);
    }

    if resp["pending_apply"].as_bool().unwrap_or(false) {
        if args.apply {
            apply(&client, &server, &args.project).await?;
        } else {
            println!();
            println!(
                "  {} Changes apply on the next container start. Apply now with {}.",
                "→".yellow(),
                format!("l8b env push --apply --project {}", args.project).cyan()
            );
        }
    }
    Ok(())
}

async fn apply(client: &reqwest::Client, server: &str, project: &str) -> Result<()> {
    println!("{} Recreating container to apply changes…", "→".yellow());
    auth::api_post_json(client, server, &format!("/projects/{project}/recreate"), &json!({})).await?;
    let final_status = status::poll_project_status(client, server, project, 120, false).await?;
    match final_status {
        Some(litebin_common::types::ProjectStatus::Running) | Some(litebin_common::types::ProjectStatus::Completed) => {
            println!("{} Applied — project is running.", "✓".green());
            Ok(())
        }
        Some(litebin_common::types::ProjectStatus::Error) => {
            bail!("recreate failed; check `l8b status --project {project}`")
        }
        _ => {
            println!("{}", "Still starting. Check `l8b status --project {project}`.".yellow());
            Ok(())
        }
    }
}

fn print_env(project: &str, env: &serde_json::Value) {
    let vars = env["vars"].as_array().cloned().unwrap_or_default();
    println!();
    if vars.is_empty() {
        println!("  {} {} (no variables set)", "Env:".dimmed(), project.cyan());
    } else {
        println!("  {} {} ({} variable(s), values are write-only)", "Env:".dimmed(), project.cyan(), vars.len());
        for v in &vars {
            let key = v["key"].as_str().unwrap_or("?");
            let masked = v["masked"].as_str().unwrap_or("");
            let shown = if masked.is_empty() { "(empty)".dimmed().to_string() } else { masked.yellow().to_string() };
            println!("    {} = {}", key.cyan(), shown);
        }
    }
    if env["pending_apply"].as_bool().unwrap_or(false) {
        println!();
        println!("  {} Pending: .env differs from what the running container was started with.", "!".yellow());
        println!("  {} Restart to apply: {}", "→".dimmed(), format!("l8b env push --apply --project {project}").cyan());
    }
    println!();
}
