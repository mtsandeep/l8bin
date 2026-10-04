//! Project verbs: list, logs, url, stop, start, restart, delete.
//! All accept a deploy token (scope: read for queries, manage for lifecycle,
//! admin for delete) and support `--json`.

use anyhow::{Result, bail};
use colored::Colorize;
use serde::Serialize;

use crate::auth;
use crate::config;
use crate::out::Out;
use crate::status;

// ── list ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ListResult {
    projects: Vec<ProjectSummary>,
}

#[derive(Serialize, Clone)]
struct ProjectSummary {
    project_id: String,
    name: String,
    status: String,
    background: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    services: usize,
}

pub(crate) async fn list(server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    let projects = auth::api_get(&client, &server, "/projects").await?;
    let stats = auth::api_get(&client, &server, "/projects/stats").await?;
    let domain = auth::fetch_platform_domain(&client, &server).await;

    let stats_by_id: std::collections::HashMap<String, serde_json::Value> = stats["stats"]
        .as_array()
        .map(|items| {
            items.iter().filter_map(|s| s["project_id"].as_str().map(|id| (id.to_string(), s.clone()))).collect()
        })
        .unwrap_or_default();

    let mut summaries: Vec<ProjectSummary> = projects
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|p| {
            let id = p["id"].as_str().unwrap_or("?").to_string();
            let background = p["is_background"].as_bool().unwrap_or(false);
            let stat = stats_by_id.get(&id);
            let status = stat
                .and_then(|s| s["status"].as_str())
                .or_else(|| p["status"].as_str())
                .unwrap_or("unknown")
                .to_string();
            let services = stat
                .and_then(|s| s["services"].as_array().map(Vec::len))
                .or_else(|| p["service_count"].as_u64().map(|c| c as usize))
                .unwrap_or(if background { 0 } else { 1 });
            let url = if background {
                None
            } else if let Some(d) = p["custom_domain"].as_str().filter(|s| !s.is_empty()) {
                Some(format!("https://{d}"))
            } else {
                Some(auth::project_live_url(&id, &domain))
            };
            ProjectSummary {
                name: p["name"].as_str().unwrap_or(&id).to_string(),
                project_id: id,
                status,
                background,
                url,
                services,
            }
        })
        .collect();

    summaries.sort_by(|a, b| {
        let running = |s: &ProjectSummary| matches!(s.status.as_str(), "running" | "completed");
        running(b).cmp(&running(a)).then_with(|| a.project_id.cmp(&b.project_id))
    });

    out.ok(&ListResult { projects: summaries.clone() });
    if !out.json {
        if summaries.is_empty() {
            println!("{}", "No projects.".dimmed());
        } else {
            println!();
            for p in &summaries {
                let status_colored = match p.status.as_str() {
                    "running" => p.status.green(),
                    "stopped" => p.status.dimmed(),
                    "deploying" | "importing" => p.status.yellow(),
                    "error" => p.status.red(),
                    s => s.normal(),
                };
                let bg_tag = if p.background { " (background)".dimmed() } else { "".dimmed() };
                let url = p.url.clone().map(|u| format!("  {u}")).unwrap_or_default();
                println!(
                    "  {} {}{}  {} service(s){}",
                    p.project_id.cyan(),
                    status_colored,
                    bg_tag,
                    p.services.to_string().dimmed(),
                    url.dimmed()
                );
            }
            println!();
        }
    }
    Ok(())
}

// ── logs ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LogsPayload {
    project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    lines: Vec<String>,
}

pub(crate) struct LogsArgs {
    pub project: String,
    pub tail: usize,
    pub service: Option<String>,
    pub deploy: bool,
}

pub(crate) async fn logs(args: LogsArgs, server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    let path = if args.deploy {
        format!("/projects/{}/deploy-logs", args.project)
    } else {
        let mut path = format!("/projects/{}/logs?tail={}", args.project, args.tail);
        if let Some(svc) = &args.service {
            path.push_str(&format!("&service={svc}"));
        }
        path
    };
    let resp = auth::api_get(&client, &server, &path).await?;

    let payload = LogsPayload {
        project_id: args.project.clone(),
        service: resp["service_name"].as_str().map(str::to_string).or(args.service),
        lines: resp["lines"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    };

    out.ok(&payload);
    if !out.json {
        println!();
        let label = payload.service.as_deref().unwrap_or("app");
        println!("  {} {} — {} {}", "---".dimmed(), args.project.cyan(), label.dimmed(), "logs".dimmed());
        println!();
        if payload.lines.is_empty() {
            println!("  {}", "(no log output)".dimmed());
        } else {
            for line in &payload.lines {
                println!("  {line}");
            }
        }
        println!();
    }
    Ok(())
}

// ── url ──────────────────────────────────────────────────────────────────────

pub(crate) async fn url(project: String, server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    let result = status::build_status_result(&client, &server, &project).await?;
    out.ok(&serde_json::json!({"project_id": result.project_id, "url": result.url}));
    if !out.json {
        match &result.url {
            Some(u) => println!("{u}"),
            None => println!("{}", "No managed URL (background project)".dimmed()),
        }
    }
    Ok(())
}

// ── lifecycle: stop / start / restart ────────────────────────────────────────

#[derive(Serialize)]
struct ActionResult {
    project_id: String,
    status: String,
}

fn report(out: &Out, project: &str, final_status: Option<String>) -> i32 {
    let status_label = final_status.clone().unwrap_or_else(|| "unknown (timed out)".to_string());
    out.ok(&ActionResult { project_id: project.to_string(), status: status_label.clone() });
    if !out.json {
        println!("{project}: {status_label}");
    }
    if matches!(status_label.as_str(), "running" | "completed" | "stopped") { 0 } else { 1 }
}

pub(crate) async fn stop(
    project: String,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    if let Err(e) =
        auth::api_post_json(&client, &server, &format!("/projects/{project}/stop"), &serde_json::json!({})).await
    {
        // Idempotent: stopping something already stopped is success.
        if format!("{e:#}").contains("project is not running") {
            std::process::exit(report(out, &project, Some("stopped".to_string())));
        }
        return Err(e);
    }

    // Stopping is async; wait until it settles (stopped or errored), max 60s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut current = "unknown (timed out)".to_string();
    loop {
        if let Ok(resp) = auth::api_get(&client, &server, &format!("/projects/{project}/stats")).await
            && let Some(s) = resp["status"].as_str()
        {
            current = s.to_string();
            if matches!(current.as_str(), "stopped" | "error" | "completed") {
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    std::process::exit(report(out, &project, Some(current)));
}

pub(crate) async fn start(
    project: String,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    auth::api_post_json(&client, &server, &format!("/projects/{project}/start"), &serde_json::json!({})).await?;
    let final_status = status::poll_project_status(&client, &server, &project, 120, out.json).await?;
    let label = final_status.map(|s| s.to_string().to_lowercase());
    std::process::exit(report(out, &project, label));
}

/// Restart = recreate: rebuilds the containers from the staged image/compose
/// and picks up pending `.env` changes.
pub(crate) async fn restart(
    project: String,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    auth::api_post_json(&client, &server, &format!("/projects/{project}/recreate"), &serde_json::json!({})).await?;
    let final_status = status::poll_project_status(&client, &server, &project, 120, out.json).await?;
    let label = final_status.map(|s| s.to_string().to_lowercase());
    std::process::exit(report(out, &project, label));
}

// ── domain ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct DomainResult {
    project_id: String,
    custom_domain: Option<String>,
    url: Option<String>,
}

/// `l8b domain set <project> <domain>` (empty/`--remove` clears it).
pub(crate) async fn domain_set(
    project: String,
    domain: Option<String>,
    remove: bool,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    if domain.is_none() && !remove {
        bail!("pass a domain, or --remove to clear the custom domain");
    }
    let value = if remove {
        String::new()
    } else {
        domain
            .unwrap()
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    };

    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    auth::api_patch_json(
        &client,
        &server,
        &format!("/projects/{project}/settings"),
        &serde_json::json!({ "custom_domain": value }),
    )
    .await?;

    let url = if value.is_empty() {
        let domain = auth::fetch_platform_domain(&client, &server).await;
        Some(auth::project_live_url(&project, &domain))
    } else {
        Some(format!("https://{value}"))
    };
    out.ok(&DomainResult {
        project_id: project.clone(),
        custom_domain: if value.is_empty() { None } else { Some(value.clone()) },
        url: url.clone(),
    });
    if !out.json {
        match url {
            Some(u) => {
                println!("{project}: {u}");
                if !value.is_empty() {
                    println!("{}", "  Point a CNAME/A record at this server (or rely on cloudflare_dns mode) — LiteBin provisions TLS automatically.".dimmed());
                }
            }
            None => println!("{project}: no managed URL"),
        }
    }
    Ok(())
}

// ── delete ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct DeleteResult {
    project_id: String,
    deleted: bool,
}

pub(crate) async fn delete(
    project: String,
    yes: bool,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
    ci_mode: &crate::ci::CiMode,
) -> Result<()> {
    if !yes {
        if out.json || ci_mode.enabled {
            bail!(crate::out::fail(
                format!("refusing to delete '{project}' without confirmation"),
                format!("re-run with --yes: l8b delete {project} --yes")
            ));
        }
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!("Delete '{project}'? Removes containers and volumes"))
            .default(false)
            .interact()
            .unwrap_or(false);
        if !confirmed {
            out.note("Aborted.");
            return Ok(());
        }
    }

    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    auth::api_delete(&client, &server, &format!("/projects/{project}")).await?;
    out.ok(&DeleteResult { project_id: project.clone(), deleted: true });
    out.note(&format!("{} Deleted '{project}'.", "✓".green()));
    Ok(())
}
