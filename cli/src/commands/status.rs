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

    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

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

    let has_session = auth::load_session().is_some();
    let cfg = config::CliConfig::load(server_flag, token_flag)?;
    let has_token = cfg.token.is_some();

    if !has_session && !has_token {
        println!();
        println!("  {}", "Not logged in.".dimmed());
        println!();
        println!("  Log in with:");
        println!("    {}", "l8b login --server <url>".cyan());
        println!("    {}", "l8b config set --token <token>".cyan());
    } else {
        let auth_method = if has_token { "token" } else { "session" };

        match auth::resolve_server(&cfg) {
            Ok(server) => {
                println!();
                println!("  {} {}", "Server:".dimmed(), server.cyan());

                if has_token {
                    let client = auth::authenticated_client(&cfg)?;
                    match auth::api_get(&client, &server, "/whoami").await {
                        Ok(me) => {
                            let name = me["name"].as_str().unwrap_or("unnamed");
                            let scope = me["scope"].as_str().unwrap_or("?");
                            let label = match me["project_id"].as_str() {
                                Some(p) => format!("token '{name}' (scope {scope}, project '{p}')"),
                                None => format!("token '{name}' (scope {scope})"),
                            };
                            println!("  {} {}", "Auth:".dimmed(), label.green());
                        }
                        Err(_) => println!("  {} {}", "Auth:".dimmed(), auth_method.green()),
                    }
                } else {
                    println!("  {} {}", "Auth:".dimmed(), auth_method.green());
                }

                if has_session {
                    let client = auth::authenticated_client(&cfg)?;
                    if let Ok(resp) = auth::session_get(&client, &server, "/status").await {
                        if let Some(ver) = resp["version"].as_str() {
                            println!("  {} {}", "Server version:".dimmed(), ver.cyan());
                        }

                        let user = &resp["user"];
                        let username = user["username"].as_str().unwrap_or("unknown");
                        let email = user["email"].as_str();
                        let is_admin = user["is_admin"].as_bool().unwrap_or(false);
                        let user_label = if let Some(email) = email {
                            format!("{} ({})", username, email)
                        } else {
                            username.to_string()
                        };
                        let admin_tag = if is_admin { " [admin]" } else { "" };
                        println!("  {} {}{}", "User:".dimmed(), user_label.cyan(), admin_tag.yellow());

                        if let Some(nodes) = resp["nodes"].as_array() {
                            println!("  {} {}", "Nodes:".dimmed(), nodes.len().to_string().cyan());
                            for node in nodes {
                                let name = node["name"].as_str().unwrap_or("?");
                                let node_status = node["status"].as_str().unwrap_or("?");
                                let version = node["version"].as_str().unwrap_or("?");
                                let arch = node["architecture"].as_str().unwrap_or("?");
                                let status_color =
                                    if node_status == "online" { node_status.green() } else { node_status.dimmed() };
                                println!(
                                    "    {}  {}  {}",
                                    name.cyan(),
                                    status_color,
                                    format!("v{} ({})", version, arch).dimmed()
                                );
                            }
                        }

                        if let Some(count) = resp["project_count"].as_i64() {
                            println!("  {} {}", "Projects:".dimmed(), count.to_string().cyan());
                        }
                    }
                }
            }
            Err(_) => {
                println!();
                println!("  {} {}", "Server:".dimmed(), "(not configured)".dimmed());
                println!("  {} {}", "Auth:".dimmed(), auth_method.green());
            }
        }
    }

    Ok(())
}
