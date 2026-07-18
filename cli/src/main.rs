mod auth;
mod build;
mod ci;
mod config;
mod deploy;
mod mise;
mod railpack;
mod ship;
mod status;
mod upload;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use litebin_common::types::ProjectStatus;

#[derive(Parser)]
#[command(name = "l8b", version, about = "LiteBin CLI — deploy apps from your terminal")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Server URL (default: from env L8B_SERVER or stored config)
    #[arg(long, env = "L8B_SERVER", global = true)]
    server: Option<String>,

    /// Deploy token (default: from env L8B_TOKEN or stored config)
    #[arg(long, env = "L8B_TOKEN", global = true)]
    token: Option<String>,

    /// CI mode: suppress verbose output and hide secrets (or set L8B_CI=true)
    #[arg(long, env = "L8B_CI", global = true)]
    ci: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy the current directory to LiteBin
    Deploy {
        /// Project ID (used as subdomain)
        #[arg(long)]
        project: String,

        /// Internal port the app listens on
        #[arg(long, default_value = "3000")]
        port: u16,

        /// Run as a background project with no managed HTTP URL
        #[arg(long)]
        background: bool,

        /// Path to project directory (default: current dir)
        #[arg(long, default_value = ".")]
        path: std::path::PathBuf,

        /// Target node ID (optional)
        #[arg(long)]
        node: Option<String>,

        /// Dockerfile path (auto-detected if not specified)
        #[arg(long)]
        dockerfile: Option<String>,

        /// Custom command to run in the container
        #[arg(long)]
        cmd: Option<String>,

        /// Memory limit in MB
        #[arg(long)]
        memory: Option<i64>,

        /// CPU limit (0.0 - 1.0)
        #[arg(long)]
        cpu: Option<f64>,

        /// Disable auto-stop
        #[arg(long)]
        no_auto_stop: bool,

        /// Pass a local file (e.g. .env) as a Docker build secret (id=l8b_env)
        #[arg(long)]
        secret: Vec<std::path::PathBuf>,

        /// Force compose mode (auto-detected if a compose file exists)
        #[arg(long)]
        compose: bool,

        /// Deploy only specific services (repeatable, compose mode only)
        #[arg(long)]
        service: Vec<String>,

        /// Grant a project capability for this deploy (repeatable: docker-access, raw-ports)
        #[arg(long = "grant-capability")]
        grant_capability: Vec<String>,
    },
    /// Interactive deploy — guided flow for new or existing projects
    Ship {
        /// Path to project directory (default: current dir)
        #[arg(long, default_value = ".")]
        path: std::path::PathBuf,

        /// App port (default: 3000)
        #[arg(long)]
        port: Option<u16>,

        /// Pass a local file (e.g. .env) as a Docker build secret (id=l8b_env)
        #[arg(long)]
        secret: Vec<std::path::PathBuf>,
    },
    /// Log in to a LiteBin server
    Login {
        /// Server URL
        #[arg(long)]
        server: String,
    },
    /// Log out (clear stored session)
    Logout,
    /// Show CLI status and server info
    Status {
        /// Show status of a specific project
        #[arg(long, short)]
        project: Option<String>,
    },
    /// Clean up leftover build artifacts (.env backups, temp dockerignore files)
    Cleanup {
        /// Project directory (default: current directory)
        #[arg(default_value = ".")]
        path: String,
    },
    /// Manage CLI configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set configuration values
    Set {
        /// Server URL
        #[arg(long)]
        server: Option<String>,
        /// Deploy token
        #[arg(long)]
        token: Option<String>,
    },
    /// Show current configuration
    Show,
}

#[tokio::main]
async fn main() -> Result<()> {
    // --generate-markdown: print CLI docs and exit (for docs generation)
    // Check before clap parsing to avoid requiring a subcommand
    if std::env::args().any(|a| a == "--generate-markdown") {
        println!("{}", clap_markdown::help_markdown::<Cli>());
        return Ok(());
    }

    let cli = Cli::parse();
    let ci_mode = ci::CiMode::from_flag(cli.ci);

    // Register secrets with GitHub Actions log masking
    if let Some(ref t) = cli.token {
        ci_mode.mask_secret(t);
    } else if let Ok(t) = std::env::var("L8B_TOKEN") {
        ci_mode.mask_secret(&t);
    }
    if let Some(ref s) = cli.server {
        ci_mode.mask_secret(s);
    } else if let Ok(s) = std::env::var("L8B_SERVER") {
        ci_mode.mask_secret(&s);
    }

    match cli.command {
        Commands::Deploy {
            project,
            port,
            background,
            path,
            node,
            dockerfile,
            cmd,
            memory,
            cpu,
            no_auto_stop,
            secret,
            compose,
            service,
            grant_capability,
        } => {
            if !project
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                bail!("Project name must only contain lowercase letters, numbers, and hyphens");
            }

            let cfg = config::CliConfig::load(cli.server.as_deref(), cli.token.as_deref())?;

            let client = auth::authenticated_client(&cfg)?;
            let server = auth::resolve_server(&cfg)?;

            // Resolve effective node: project's sticky node_id takes precedence over --node flag
            let existing_project =
                auth::session_get(&client, &server, &format!("/projects/{}", project)).await.ok();
            let effective_node = if let Some(proj_json) = existing_project.as_ref() {
                let existing_node = proj_json.get("node_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                if existing_node.is_some() && node.is_some() && existing_node != node.as_deref() {
                    eprintln!("  Note: --node ignored, project is pinned to node '{}'", existing_node.unwrap());
                }
                existing_node.or(node.as_deref()).map(|s| s.to_string())
            } else {
                node.clone()
            };
            let effective_background = background
                || existing_project
                    .as_ref()
                    .and_then(|project| project.get("is_background"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

            // Check for compose file (auto-detect or forced via --compose)
            let compose_file = ship::detect_compose_file(&path);
            if compose || compose_file.is_some() {
                let compose_name = compose_file.unwrap_or_else(|| {
                    if compose {
                        // Find it now
                        ship::detect_compose_file(&path).expect("no compose file found")
                    } else {
                        unreachable!()
                    }
                });

                if compose {
                    if compose_file.is_none() {
                        bail!("--compose flag specified but no compose file found in {}", path.display());
                    }
                }

                let target_services = if service.is_empty() { None } else { Some(service) };

                // Resolve target platform from node architecture
                let nodes = auth::fetch_online_nodes(&client, &server).await;
                let platform = ship::resolve_platform(&nodes, effective_node.as_deref());

                ship::deploy_compose_noninteractive(
                    &client, &server, &project, &path, compose_name, true,
                    ship::ComposeDeployOpts {
                        target_services,
                        node_id: effective_node.clone(),
                        grant_capabilities: grant_capability,
                        is_background: effective_background,
                    },
                    platform.as_deref(),
                ).await?;

                // Poll for completion (2 min timeout, non-interactive)
                let final_status = status::poll_project_status(&client, &server, &project, 120).await?;
                match final_status.as_ref() {
                    Some(ProjectStatus::Running | ProjectStatus::Completed) => {
                        if effective_background {
                            println!("Deployed! No managed URL (background project).");
                        } else {
                            let domain = auth::fetch_platform_domain(&client, &server).await;
                            let url = auth::project_live_url(&project, &domain);
                            println!("Deployed! {}", url);
                        }
                    }
                    Some(ProjectStatus::Error) => {
                        println!("Deploy failed for project '{}'.", project);
                        std::process::exit(1);
                    }
                    _ => {
                        println!("Deployment is still in progress.");
                        println!("Run {} to check status.", format!("l8b status --project {}", project).cyan());
                    }
                }
            } else {
                let image_tag = format!("{}/{}:latest", config::IMAGE_PREFIX, project);

                // Resolve target platform from node architecture
                let nodes = auth::fetch_online_nodes(&client, &server).await;
                let platform = ship::resolve_platform(&nodes, effective_node.as_deref());

                let image = build::build_project(&path, dockerfile.as_deref(), &image_tag, secret, ci_mode.enabled, platform.as_deref()).await?;

                ci_mode.println("Uploading image...");
                let image_id = upload::upload_tar(
                    &client,
                    &server,
                    &project,
                    std::path::Path::new(&image.path),
                    &image.image_id,
                    effective_node.as_deref(),
                    ci_mode.enabled,
                )
                .await?;

                ci_mode.println("Deploying...");
                let response = deploy::deploy_or_redeploy(
                    &client,
                    &server,
                    &project,
                    &image_id,
                    if effective_background { None } else { Some(port) },
                    effective_background,
                    effective_node.as_deref(),
                    cmd.as_deref(),
                    memory,
                    cpu,
                    !no_auto_stop,
                )
                .await?;

                if response.status == ProjectStatus::Deploying {
                    // Poll for completion (2 min timeout, non-interactive)
                    let final_status = status::poll_project_status(&client, &server, &project, 120).await?;
                    match final_status.as_ref() {
                        Some(ProjectStatus::Running | ProjectStatus::Completed) => {
                        if effective_background {
                            println!("Deployed! No managed URL (background project).");
                            } else {
                            let url = if let Some(u) = response.url.as_deref().filter(|u| !u.is_empty()) {
                                u.to_string()
                            } else {
                                let domain = auth::fetch_platform_domain(&client, &server).await;
                                auth::project_live_url(&project, &domain)
                            };
                            println!("Deployed! {}", url);
                        }
                        }
                        Some(ProjectStatus::Error) => {
                            println!("Deploy failed for project '{}'.", project);
                            std::process::exit(1);
                        }
                        _ => {
                            println!("Deployment is still in progress.");
                            println!("Run {} to check status.", format!("l8b status --project {}", project).cyan());
                        }
                    }
                } else {
                    match response.url.as_deref() {
                        Some(url) => println!("Deployed! {}", url),
                        None => println!("Deployed! No managed URL (background project)."),
                    }
                }

                // Clean up
                let _ = std::fs::remove_file(&image.path);
            }
        }
        Commands::Ship { path, port, secret } => {
            if ci_mode.enabled {
                bail!("'ship' is an interactive command and cannot be used in CI mode. Use 'deploy' instead.");
            }
            let cfg = config::CliConfig::load(cli.server.as_deref(), None)?;
            if auth::load_session().is_none() {
                let server = dialoguer::Input::<String>::new()
                    .with_prompt("Server URL")
                    .default(cfg.server.clone().unwrap_or_default())
                    .interact_text()?;
                auth::login(&server).await?;
            }
            let cfg = config::CliConfig::load(cli.server.as_deref(), None)?;
            let client = auth::authenticated_client(&cfg)?;
            let server = auth::resolve_server(&cfg)?;
            ship::run(&client, &server, Some(path.to_str().unwrap_or(".")), port, secret).await?;
        }
        Commands::Login { server } => {
            auth::login(&server).await?;
        }
        Commands::Logout => {
            auth::clear_session()?;
            println!("Logged out.");
        }
        Commands::Status { project } => {
            if let Some(project_id) = project {
                // Show specific project status
                let cfg = config::CliConfig::load(cli.server.as_deref(), cli.token.as_deref())?;
                let client = auth::authenticated_client(&cfg)?;
                let server = auth::resolve_server(&cfg)?;
                status::show_project_status(&client, &server, &project_id).await?;
            } else {
                // Show system status (original behavior)
                println!("l8b v{}", env!("CARGO_PKG_VERSION"));

                let has_session = auth::load_session().is_some();
                let cfg = config::CliConfig::load(cli.server.as_deref(), cli.token.as_deref())?;
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
                                            let status_color = if status == "online" { status.green() } else { status.dimmed() }; // status from JSON string, not enum
                                            println!("    {}  {}  {}", name.cyan(), status_color, format!("v{} ({})", version, arch).dimmed());
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
        }
        Commands::Cleanup { path } => {
            let dir = std::path::Path::new(&path);
            build::cleanup_build_artifacts(dir)?;
        }
        Commands::Config { action } => match action {
            ConfigAction::Set { server, token } => {
                if let Some(ref t) = token {
                    ci_mode.mask_secret(t);
                }
                if let Some(ref s) = server {
                    ci_mode.mask_secret(s);
                }
                config::CliConfig::save(server.as_deref(), token.as_deref())?;
                if ci_mode.enabled {
                    println!("Config saved.");
                } else {
                    println!("Config saved to {}", config::CliConfig::config_path().display());
                }
            }
            ConfigAction::Show => {
                config::CliConfig::show(ci_mode.enabled)?;
            }
        },
    }

    Ok(())
}
