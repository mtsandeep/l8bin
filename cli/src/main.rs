mod auth;
mod build;
mod ci;
mod commands;
mod config;
mod deploy;
mod mise;
mod out;
mod project_config;
mod railpack;
mod ship;
mod status;
mod tls;
mod upload;

use anyhow::{Result, bail};
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

    /// Machine-readable JSON output (single object on stdout; or set L8B_JSON=1)
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
// Deploy's flag set dwarfs the other variants; constructed once per invocation.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Deploy the current directory to LiteBin
    Deploy {
        /// Project ID (default: `project` from l8b.toml)
        #[arg(long)]
        project: Option<String>,

        /// Internal port the app listens on (default: l8b.toml `port`, else 3000)
        #[arg(long)]
        port: Option<u16>,

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

        /// Push this file as runtime env after deploying (KEY=VALUE lines, merged)
        #[arg(long)]
        env_file: Option<std::path::PathBuf>,

        /// Force compose mode (auto-detected if a compose file exists)
        #[arg(long)]
        compose: bool,

        /// Deploy only specific services (repeatable, compose mode only)
        #[arg(long)]
        service: Vec<String>,

        /// Grant a project capability for this deploy (repeatable: docker-observe, host-network, raw-ports)
        #[arg(long = "grant-capability")]
        grant_capability: Vec<String>,

        /// How to upload the image to the node: auto (default, direct when the node
        /// supports it), direct (client → agent, skipping the master relay), or relay
        /// (client → master → agent).
        #[arg(long, value_enum, default_value_t = upload::UploadMode::Auto)]
        upload: upload::UploadMode,
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
    /// List all projects with live status
    List,
    /// Show container logs for a project
    Logs {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
        /// Number of lines to show
        #[arg(long, default_value_t = 100)]
        tail: usize,
        /// Service name (multi-service projects; defaults to the public service)
        #[arg(long)]
        service: Option<String>,
        /// Show deploy logs instead of container logs
        #[arg(long)]
        deploy: bool,
    },
    /// Print the project's managed URL
    Url {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
    },
    /// Stop a running project
    Stop {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
    },
    /// Start a stopped project
    Start {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
    },
    /// Recreate containers (applies pending .env changes)
    Restart {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
    },
    /// Delete a project, its containers, and its volumes
    Delete {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
        /// Skip the confirmation prompt (required in CI/JSON mode)
        #[arg(long)]
        yes: bool,
    },
    /// Write l8b.toml project defaults (and optionally the workspace .mcp.json)
    Init {
        /// Project ID (used as subdomain)
        #[arg(long)]
        project: Option<String>,
        /// Internal port the app listens on
        #[arg(long)]
        port: Option<u16>,
        /// Target node ID
        #[arg(long)]
        node: Option<String>,
        /// Default env file for `deploy --env-file`
        #[arg(long)]
        env_file: Option<String>,
        /// Also write a workspace .mcp.json for the litebin MCP server
        #[arg(long)]
        mcp: bool,
        /// Overwrite an existing l8b.toml / .mcp.json
        #[arg(long)]
        force: bool,
        /// Project directory (default: current dir)
        #[arg(long, default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Environment sanity checks with recovery hints
    Doctor,
    /// First-run bootstrap: create the admin account and pair this machine
    Setup {
        /// Server URL
        #[arg(long)]
        server: String,
    },
    /// Manage a project's custom domain
    Domain {
        #[command(subcommand)]
        action: DomainAction,
    },
    /// Log in to a LiteBin server (dashboard approval or username/password)
    Login {
        /// Server URL
        #[arg(long)]
        server: String,
        /// Scope to request: read, deploy, manage (default), or admin — the approver decides
        #[arg(long)]
        scope: Option<String>,
        /// Skip the menu and pair via dashboard approval
        #[arg(long, conflicts_with = "password")]
        pair: bool,
        /// Skip the menu and log in with username/password (session)
        #[arg(long)]
        password: bool,
    },
    /// Log out (clear stored session)
    Logout,
    /// Show CLI status and server info
    Status {
        /// Show status of a specific project
        #[arg(long, short)]
        project: Option<String>,
        /// Wait until the project reaches a terminal state; exit 0 only if running
        #[arg(long)]
        wait: bool,
        /// Wait timeout in seconds (default 120, with --wait)
        #[arg(long)]
        timeout: Option<u64>,
        /// Probe the project URL for a 2xx (implies --wait; skipped for background projects)
        #[arg(long)]
        healthy: bool,
    },
    /// Clean up leftover build artifacts (.env backups, temp dockerignore files)
    Cleanup {
        /// Project directory (default: current directory)
        #[arg(default_value = ".")]
        path: String,
    },
    /// Manage runtime environment variables (values are write-only)
    Env {
        #[command(subcommand)]
        action: EnvAction,
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

#[derive(Subcommand)]
enum DomainAction {
    /// Set a custom domain
    Set {
        /// Custom domain (e.g. myapp.example.com)
        domain: String,
        /// Project ID (default: l8b.toml `project`)
        #[arg(long, short)]
        project: Option<String>,
    },
    /// Clear the custom domain
    Remove {
        /// Project ID (default: l8b.toml `project`)
        #[arg(long, short)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum EnvAction {
    /// List env keys with masked previews (never values)
    List {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
    },
    /// Push env vars from a file or stdin (never from arguments)
    Push {
        /// Project ID (default: l8b.toml `project`)
        project: Option<String>,
        /// Env file to push (default: .env in the current directory)
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Read env content from stdin instead of a file (e.g. `sops -d … | l8b env push --stdin`)
        #[arg(long)]
        stdin: bool,
        /// Replace the whole env file instead of merging into it
        #[arg(long)]
        replace: bool,
        /// Recreate the container after pushing to apply changes
        #[arg(long)]
        apply: bool,
    },
}

#[tokio::main]
async fn main() {
    // --generate-markdown: print CLI docs and exit (for docs generation)
    // Check before clap parsing to avoid requiring a subcommand
    if std::env::args().any(|a| a == "--generate-markdown") {
        println!("{}", clap_markdown::help_markdown::<Cli>());
        return;
    }

    let cli = Cli::parse();
    let out = out::Out::from_flag(cli.json);
    // JSON output is machine-consumed: keep stdout to the single JSON object.
    let ci_mode = ci::CiMode::from_flag(cli.ci || out.json);

    if let Err(e) = run(cli, &out, &ci_mode).await {
        let (message, hint) = out::split_hint(&e);
        if out.json {
            println!("{}", serde_json::json!({"ok": false, "error": {"message": message, "hint": hint}}));
        } else {
            eprintln!("Error: {message}");
            if let Some(h) = hint {
                eprintln!("Hint: {h}");
            }
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli, out: &out::Out, ci_mode: &ci::CiMode) -> Result<()> {
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
            env_file,
            compose,
            service,
            grant_capability,
            upload,
        } => {
            let defaults = project_config::load(&path);
            let project = project_config::resolve_project(project.as_deref(), &path)?;
            let port = port.or_else(|| defaults.as_ref().and_then(|c| c.port)).unwrap_or(3000);
            let node = node.or_else(|| defaults.as_ref().and_then(|c| c.node.clone()));
            let env_file =
                env_file.or_else(|| defaults.as_ref().and_then(|c| c.env_file.clone()).map(std::path::PathBuf::from));
            commands::deploy::run(
                commands::deploy::DeployArgs {
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
                    env_file,
                    compose,
                    service,
                    grant_capability,
                    upload,
                },
                cli.server.as_deref(),
                cli.token.as_deref(),
                ci_mode,
                out,
            )
            .await?;
        }
        Commands::Ship { path, port, secret } => {
            if ci_mode.enabled {
                bail!(crate::out::fail(
                    "'ship' is interactive and cannot run in CI/JSON mode",
                    "use `l8b deploy --project <id>`"
                ));
            }
            let cfg = config::CliConfig::load(cli.server.as_deref(), None)?;
            if auth::load_session().is_none() {
                let server = dialoguer::Input::<String>::new()
                    .with_prompt("Server URL")
                    .default(cfg.server.clone().unwrap_or_default())
                    .interact_text()?;
                auth::login(&server, "manage").await?;
            }
            let cfg = config::CliConfig::load(cli.server.as_deref(), None)?;
            let client = auth::authenticated_client(&cfg)?;
            let server = auth::resolve_server(&cfg)?;
            ship::run(&client, &server, Some(path.to_str().unwrap_or(".")), port, secret, cfg.token.is_some()).await?;
        }
        Commands::Init { project, port, node, env_file, mcp, force, path } => {
            commands::init::run(
                commands::init::InitArgs { project, port, node, env_file, mcp, force, path },
                ci_mode,
                out,
            )
            .await?;
        }
        Commands::Doctor => {
            commands::doctor::run(cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Setup { server } => {
            commands::setup::run(&server, ci_mode, out).await?;
        }
        Commands::Domain { action } => match action {
            DomainAction::Set { domain, project } => {
                let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
                commands::projects::domain_set(
                    project,
                    Some(domain),
                    false,
                    cli.server.as_deref(),
                    cli.token.as_deref(),
                    out,
                )
                .await?;
            }
            DomainAction::Remove { project } => {
                let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
                commands::projects::domain_set(project, None, true, cli.server.as_deref(), cli.token.as_deref(), out)
                    .await?;
            }
        },
        Commands::List => {
            commands::projects::list(cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Logs { project, tail, service, deploy } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::logs(
                commands::projects::LogsArgs { project, tail, service, deploy },
                cli.server.as_deref(),
                cli.token.as_deref(),
                out,
            )
            .await?;
        }
        Commands::Url { project } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::url(project, cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Stop { project } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::stop(project, cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Start { project } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::start(project, cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Restart { project } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::restart(project, cli.server.as_deref(), cli.token.as_deref(), out).await?;
        }
        Commands::Delete { project, yes } => {
            let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
            commands::projects::delete(project, yes, cli.server.as_deref(), cli.token.as_deref(), out, ci_mode).await?;
        }
        Commands::Login { server, scope, pair, password } => {
            let use_password = if password {
                true
            } else if pair {
                false
            } else if ci_mode.enabled || out.json {
                bail!(out::fail(
                    "login needs an auth method in CI/JSON mode",
                    "use `l8b login --pair` (approve from the dashboard) or set L8B_TOKEN"
                ));
            } else {
                let methods = vec!["Approve from the dashboard (recommended)", "Username and password"];
                let choice = dialoguer::Select::new()
                    .with_prompt("How do you want to authenticate?")
                    .items(&methods)
                    .default(0)
                    .interact()?;
                choice == 1
            };
            if use_password {
                auth::login_password(&server).await?;
            } else {
                auth::login(&server, scope.as_deref().unwrap_or("manage")).await?;
            }
        }
        Commands::Logout => {
            auth::clear_session()?;
            println!("Logged out.");
        }
        Commands::Status { project, wait, timeout, healthy } => {
            let project = project.or_else(|| project_config::load(std::path::Path::new(".")).and_then(|c| c.project));
            commands::status::run(
                commands::status::StatusArgs { project, wait, timeout, healthy },
                cli.server.as_deref(),
                cli.token.as_deref(),
                out,
            )
            .await?;
        }
        Commands::Cleanup { path } => {
            let dir = std::path::Path::new(&path);
            build::cleanup_build_artifacts(dir)?;
        }
        Commands::Env { action } => match action {
            EnvAction::List { project } => {
                let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
                commands::env::list(
                    commands::env::EnvListArgs { project },
                    cli.server.as_deref(),
                    cli.token.as_deref(),
                    out,
                )
                .await?;
            }
            EnvAction::Push { project, file, stdin, replace, apply } => {
                let project = project_config::resolve_project(project.as_deref(), std::path::Path::new("."))?;
                commands::env::push(
                    commands::env::EnvPushArgs { project, file, stdin, replace, apply },
                    cli.server.as_deref(),
                    cli.token.as_deref(),
                    ci_mode,
                    out,
                )
                .await?;
            }
        },
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
