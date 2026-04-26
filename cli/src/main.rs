mod auth;
mod build;
mod ci;
mod config;
mod deploy;
mod mise;
mod railpack;
mod ship;
mod upload;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

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
            let effective_node = if let Ok(proj_json) = auth::session_get(&client, &server, &format!("/projects/{}", project)).await {
                let existing = proj_json.get("node_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                if existing.is_some() && node.is_some() && existing != node.as_deref() {
                    eprintln!("  Note: --node ignored, project is pinned to node '{}'", existing.unwrap());
                }
                existing.or(node.as_deref()).map(|s| s.to_string())
            } else {
                node.clone()
            };

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

                let url = ship::deploy_compose_noninteractive(
                    &client, &server, &project, &path, compose_name, true,
                    ship::ComposeDeployOpts { target_services, node_id: effective_node.clone() },
                ).await?;

                println!("Deployed! {}", url);
            } else {
                let image_tag = format!("{}/{}:latest", config::IMAGE_PREFIX, project);
                let image = build::build_project(&path, dockerfile.as_deref(), &image_tag, secret, ci_mode.enabled).await?;

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
                let response = deploy::deploy(
                    &client,
                    &server,
                    &project,
                    &image_id,
                    port,
                    effective_node.as_deref(),
                    cmd.as_deref(),
                    memory,
                    cpu,
                    !no_auto_stop,
                )
                .await?;

                println!("Deployed! {}", response.url);

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
