use anyhow::Result;
use colored::Colorize;

use crate::auth;
use crate::config;
use crate::out::Out;
use crate::status;

pub(crate) struct StatusArgs {
    pub project: Option<String>,
    pub wait: bool,
    pub timeout: Option<u64>,
    pub healthy: bool,
}

pub(crate) async fn run(
    args: StatusArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    out: &Out,
) -> Result<()> {
    let Some(project_id) = args.project else {
        // No project: CLI/server info (human-oriented)
        show_server_status(server_flag, token_flag).await?;
        return Ok(());
    };

    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    let client = target.client;
    let server = target.server;

    let wait = args.wait || args.healthy;
    let timeout = args.timeout.unwrap_or(120);

    if wait {
        let final_status = status::poll_project_status(&client, &server, &project_id, timeout, out.json).await?;
        if final_status.is_none() {
            let mut result = status::build_status_result(&client, &server, &project_id).await?;
            result.status = "deploying (timed out waiting)".to_string();
            finish(&result, None, wait, out);
            return Ok(());
        }
    }

    let mut result = status::build_status_result(&client, &server, &project_id).await?;

    if args.healthy {
        match (&result.url, result.background) {
            (Some(url), false) => {
                let health = status::probe_url(url).await;
                let healthy_ok = health.ok;
                result.health = Some(health);
                finish(&result, Some(healthy_ok), wait, out);
                return Ok(());
            }
            _ => {
                finish(&result, None, wait, out);
                return Ok(());
            }
        }
    }

    finish(&result, None, wait, out);
    Ok(())
}

/// Render, print JSON, and apply the exit-code contract:
/// with `wait`: 0 only when running; with `healthy_ok`: also requires a 2xx.
fn finish(result: &status::StatusResult, healthy_ok: Option<bool>, wait: bool, out: &Out) {
    out.ok(result);
    if !out.json {
        status::render_status_human(result);
        if result.status == "deploying" {
            println!();
            println!("  {} {}", "Tip:".dimmed(), "Run this command again to check for updates.".dimmed());
        }
        println!();
    }

    if !wait {
        return;
    }
    let running = matches!(result.status.as_str(), "running" | "completed");
    let healthy = healthy_ok.unwrap_or(true);
    if running && healthy {
        return;
    }
    std::process::exit(1);
}

/// `l8b status` without a project — CLI/server configuration overview.
async fn show_server_status(server_flag: Option<&str>, token_flag: Option<&str>) -> Result<()> {
    use colored::Colorize;

    println!("l8b v{}", env!("CARGO_PKG_VERSION"));

    let cfg = config::CliConfig::load(server_flag, token_flag);
    let store = config::CredentialStore::load();
    let logged_in = store.logged_in_servers();

    if logged_in.is_empty() && cfg.server.is_none() {
        println!();
        println!("  {}", "Not logged in.".dimmed());
        println!();
        println!("  Log in with:");
        println!("    {}", "l8b login --server <url>".cyan());
        return Ok(());
    }

    // The current server: flag > stored default > the single login.
    let Some(current) = cfg
        .server
        .clone()
        .map(|s| crate::config::normalize_server(&s))
        .or_else(|| store.default.clone())
        .or_else(|| logged_in.first().map(|s| s.to_string()))
    else {
        println!();
        println!("  {}", "Logged into multiple servers — no default:".dimmed());
        for s in &logged_in {
            println!("    {}", s.cyan());
        }
        println!("  Pass {} to pick one.", "--server <url>".cyan());
        return Ok(());
    };

    println!();
    println!("  {} {}", "Server:".dimmed(), current.cyan());

    let cred = store.get(&current);
    let has_session = cred.and_then(|c| c.cookie.clone()).is_some();

    if let Some(cred) = cred
        && let Some(auth) = cred.auth_header()
    {
        match auth::api_get(&auth::client_for(auth)?, &current, "/whoami").await {
            Ok(me) => {
                if me["kind"].as_str() == Some("token") {
                    let name = me["name"].as_str().unwrap_or("unnamed");
                    let scope = me["scope"].as_str().unwrap_or("?");
                    let label = match me["project_id"].as_str() {
                        Some(p) => format!("token '{name}' (scope {scope}, project '{p}')"),
                        None => format!("token '{name}' (scope {scope})"),
                    };
                    println!("  {} {}", "Auth:".dimmed(), label.green());
                } else {
                    let user = me["username"].as_str().unwrap_or("?");
                    println!("  {} {}", "Auth:".dimmed(), format!("session '{user}'").green());
                }
            }
            Err(_) => {
                let kind = if cred.token.is_some() { "token" } else { "session" };
                println!("  {} {}", "Auth:".dimmed(), kind.green());
            }
        }
    } else if cfg.token.is_some() {
        println!("  {} {}", "Auth:".dimmed(), "token (--token/L8B_TOKEN)".green());
    } else {
        println!("  {} {}", "Auth:".dimmed(), "(not logged in to this server)".dimmed());
    }

    if has_session {
        let client = auth::client_for(store.get(&current).and_then(|c| c.cookie.clone()).unwrap_or_default())?;
        if let Ok(resp) = auth::session_get(&client, &current, "/status").await {
            if let Some(ver) = resp["version"].as_str() {
                println!("  {} {}", "Server version:".dimmed(), ver.cyan());
            }

            let user = &resp["user"];
            let username = user["username"].as_str().unwrap_or("unknown");
            let email = user["email"].as_str();
            let is_admin = user["is_admin"].as_bool().unwrap_or(false);
            let user_label =
                if let Some(email) = email { format!("{} ({})", username, email) } else { username.to_string() };
            let admin_tag = if is_admin { " [admin]" } else { "" };
            println!("  {} {}{}", "User:".dimmed(), user_label.cyan(), admin_tag.yellow());

            if let Some(nodes) = resp["nodes"].as_array() {
                println!("  {} {}", "Nodes:".dimmed(), nodes.len().to_string().cyan());
                for node in nodes {
                    let name = node["name"].as_str().unwrap_or("?");
                    let node_status = node["status"].as_str().unwrap_or("?");
                    let version = node["version"].as_str().unwrap_or("?");
                    let arch = node["architecture"].as_str().unwrap_or("?");
                    let status_color = if node_status == "online" { node_status.green() } else { node_status.dimmed() };
                    println!("    {}  {}  {}", name.cyan(), status_color, format!("v{} ({})", version, arch).dimmed());
                }
            }

            if let Some(count) = resp["project_count"].as_i64() {
                println!("  {} {}", "Projects:".dimmed(), count.to_string().cyan());
            }
        }
    }

    // Other known servers.
    let others: Vec<&str> =
        logged_in.iter().filter(|s| !current.eq_ignore_ascii_case(s.as_str())).map(|s| s.as_str()).collect();
    if !others.is_empty() {
        println!("  {} {}", "Also logged into:".dimmed(), others.join(", ").dimmed());
    }

    Ok(())
}
