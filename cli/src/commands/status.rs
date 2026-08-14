use anyhow::Result;
use colored::Colorize;

use crate::auth;
use crate::config;

pub(crate) async fn run(project: Option<String>, server_flag: Option<&str>, token_flag: Option<&str>) -> Result<()> {
    if let Some(project_id) = project {
        // Show specific project status
        let cfg = config::CliConfig::load(server_flag, token_flag)?;
        let client = auth::authenticated_client(&cfg)?;
        let server = auth::resolve_server(&cfg)?;
        crate::status::show_project_status(&client, &server, &project_id).await?;
    } else {
        // Show system status (original behavior)
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
                    println!("  {} {}", "Auth:".dimmed(), auth_method.green());

                    if has_session {
                        let client = auth::authenticated_client(&cfg)?;
                        if let Ok(resp) = auth::session_get(&client, &server, "/status").await {
                            // Server version
                            if let Some(ver) = resp["version"].as_str() {
                                println!("  {} {}", "Server version:".dimmed(), ver.cyan());
                            }

                            // User
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

                            // Nodes — one per line
                            if let Some(nodes) = resp["nodes"].as_array() {
                                println!("  {} {}", "Nodes:".dimmed(), nodes.len().to_string().cyan());
                                for node in nodes {
                                    let name = node["name"].as_str().unwrap_or("?");
                                    let status = node["status"].as_str().unwrap_or("?");
                                    let version = node["version"].as_str().unwrap_or("?");
                                    let arch = node["architecture"].as_str().unwrap_or("?");
                                    let status_color =
                                        if status == "online" { status.green() } else { status.dimmed() }; // status from JSON string, not enum
                                    println!(
                                        "    {}  {}  {}",
                                        name.cyan(),
                                        status_color,
                                        format!("v{} ({})", version, arch).dimmed()
                                    );
                                }
                            }

                            // Projects
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
    }

    Ok(())
}
