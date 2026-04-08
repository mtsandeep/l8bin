mod auth;
mod build;
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
    },
    /// Interactive deploy — guided flow for new or existing projects
    Ship {
        /// Path to project directory (default: current dir)
        #[arg(long, default_value = ".")]
        path: std::path::PathBuf,

        /// App port (default: 3000)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Log in to a LiteBin server
    Login {
        /// Server URL
        #[arg(long)]
        server: String,
    },
    /// Log out (clear stored session)
    Logout,
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

            let image_tag = format!("{}/{}:latest", config::IMAGE_PREFIX, project);
            let image = build::build_project(&path, dockerfile.as_deref(), &image_tag).await?;

            println!("Uploading image...");
            let image_id = upload::upload_tar(
                &client,
                &server,
                &project,
                std::path::Path::new(&image.path),
                node.as_deref(),
            )
            .await?;

            println!("Deploying...");
            let response = deploy::deploy(
                &client,
                &server,
                &project,
                &image_id,
                port,
                node.as_deref(),
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
        Commands::Ship { path, port } => {
            let cfg = config::CliConfig::load(cli.server.as_deref(), None)?;
            let client = auth::authenticated_client(&cfg)?;
            let server = auth::resolve_server(&cfg)?;
            ship::run(&client, &server, Some(path.to_str().unwrap_or(".")), port).await?;
        }
        Commands::Login { server } => {
            auth::login(&server).await?;
        }
        Commands::Logout => {
            auth::clear_session()?;
            println!("Logged out.");
        }
        Commands::Config { action } => match action {
            ConfigAction::Set { server, token } => {
                config::CliConfig::save(server.as_deref(), token.as_deref())?;
                println!("Config saved to {}", config::CliConfig::config_path().display());
            }
            ConfigAction::Show => {
                let path = config::CliConfig::config_path();
                if path.exists() {
                    let content = std::fs::read_to_string(&path)?;
                    println!("{}", content);
                } else {
                    println!("No config found. Set with:");
                    println!("  l8b config set --server <url>");
                    println!("  l8b config set --token <token>");
                }
            }
        },
    }

    Ok(())
}
