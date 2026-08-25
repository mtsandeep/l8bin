use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::{Confirm, Input, Select};
use litebin_common::types::ProjectStatus;
use serde_json::json;

use crate::auth;

use super::ui::{
    detect_compose_file, print_live_url, print_no_managed_url, project_is_staged, resolve_app_port, short_image,
    show_env_path, spinner,
};

#[derive(serde::Deserialize)]
pub(super) struct ProjectInfo {
    pub id: String,
    pub status: ProjectStatus,
    pub node_id: Option<String>,
    #[serde(default)]
    pub is_background: bool,
    #[serde(default)]
    pub is_staged: bool,
    pub public_stats: Option<litebin_common::types::ServiceInfo>,
}

pub async fn run(
    client: &reqwest::Client,
    server: &str,
    path_override: Option<&str>,
    port_override: Option<u16>,
    secret_override: Vec<PathBuf>,
) -> Result<()> {
    let project_dir = Path::new(path_override.unwrap_or("."));

    let projects_json = auth::session_get(client, server, "/projects").await?;
    let projects: Vec<ProjectInfo> = serde_json::from_value(projects_json).unwrap_or_default();

    let choices = vec!["New project", "Existing project"];
    let selection = Select::new().with_prompt("Deploy to").items(&choices).default(0).interact()?;

    if selection == 0 {
        new_project_flow(client, server, project_dir, port_override, secret_override).await
    } else {
        existing_project_flow(client, server, project_dir, &projects, port_override, secret_override).await
    }
}

// ── New / existing project flows ─────────────────────────────────────────────

async fn new_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    port_override: Option<u16>,
    secret_override: Vec<PathBuf>,
) -> Result<()> {
    let name: String = Input::new()
        .with_prompt("Project name")
        .default("".to_string())
        .validate_with(|input: &String| -> Result<(), &str> {
            if input.is_empty() {
                return Err("Project name is required");
            }
            if !input.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                return Err("Use only lowercase letters, numbers, and hyphens");
            }
            Ok(())
        })
        .interact_text()?;

    let is_background = select_background_project()?;

    println!("  :: Creating project {}...", name.cyan());
    auth::session_post(client, server, "/projects", &json!({"id": &name, "is_background": is_background}))
        .await
        .with_context(|| format!("failed to create project '{}'", name))?;
    println!("  {} Project created", "✔".green());

    println!("  :: Generating deploy token for {}...", name.cyan());
    let token_resp =
        auth::session_post(client, server, "/deploy-tokens", &json!({"project_id": &name, "name": "cli-generated"}))
            .await?;

    let token = token_resp["token"].as_str().unwrap_or("<error>").to_string();

    println!();
    println!("  {} Deploy token generated for {}", "🔐".dimmed(), name.cyan());
    println!("  {} Save it for CI/CD:", "!".yellow());
    println!("  {}", format!("L8B_TOKEN={}", token).dimmed());
    println!();

    let port = if is_background { None } else { Some(resolve_app_port(project_dir, port_override)?) };

    let url = super::deploy::build_and_deploy(
        client,
        server,
        &name,
        project_dir,
        port,
        is_background,
        secret_override,
        true,
        None,
    )
    .await?;

    if let Some(url) = url {
        print_live_url(&url);
    } else {
        print_no_managed_url();
    }
    println!("  {} Use this token to redeploy from CI/CD:", "💡".dimmed());
    let deploy_hint = if is_background {
        format!("L8B_TOKEN={} l8b deploy --project {} --background", token, name)
    } else {
        format!(
            "L8B_TOKEN={} l8b deploy --project {} --port {}",
            token,
            name,
            port.expect("web projects have an HTTP port")
        )
    };
    println!("  {}", deploy_hint.dimmed());
    println!();

    Ok(())
}

async fn existing_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    projects: &[ProjectInfo],
    port_override: Option<u16>,
    secret_override: Vec<PathBuf>,
) -> Result<()> {
    if projects.is_empty() {
        anyhow::bail!("No existing projects found. Create one with the 'New project' option.");
    }

    let items: Vec<String> = projects
        .iter()
        .map(|p| {
            let status = match &p.status {
                ProjectStatus::Running => p.status.to_string().green().to_string(),
                ProjectStatus::Pending | ProjectStatus::Stopped | ProjectStatus::Unconfigured => {
                    p.status.to_string().yellow().to_string()
                }
                ProjectStatus::Error | ProjectStatus::Degraded => p.status.to_string().red().to_string(),
                _ => p.status.to_string(),
            };
            let image = p.public_stats.as_ref().map(|ps| short_image(&ps.image)).unwrap_or_else(|| "—".to_string());
            let port = if p.is_background {
                "background".to_string()
            } else {
                p.public_stats
                    .as_ref()
                    .and_then(|ps| ps.mapped_port)
                    .map(|p| format!("port {}", p))
                    .unwrap_or("—".to_string())
            };
            format!("  {:<20} {:<12} {:<25} {}", p.id, status, image, port)
        })
        .collect();

    let idx = Select::new().with_prompt("Select project").items(&items).interact()?;

    let project = &projects[idx];
    let project_id = &project.id;

    let staged = project.status == ProjectStatus::Unconfigured && project_is_staged(project);
    let awaiting_first_deploy =
        project.status == ProjectStatus::Pending || (project.status == ProjectStatus::Unconfigured && !staged);
    let actions: Vec<&str> = if staged {
        vec!["Resume deployment", "Redeploy", "Delete"]
    } else if awaiting_first_deploy {
        vec!["Deploy", "Delete"]
    } else {
        vec!["Redeploy", "Recreate", "Start", "Stop", "Delete"]
    };
    let action_idx = Select::new().with_prompt("Action").items(&actions).default(0).interact()?;

    match actions[action_idx] {
        "Resume deployment" => {
            let started =
                await_runtime_config_and_start(client, server, project_id, project.node_id.as_deref()).await?;
            if started {
                if project.is_background {
                    print_no_managed_url();
                } else {
                    let domain = auth::fetch_platform_domain(client, server).await;
                    let live_url = auth::project_live_url(project_id, &domain);
                    print_live_url(&live_url);
                }
            }
        }
        "Deploy" | "Redeploy" => {
            let existing_node = project.node_id.as_deref();
            let is_first = matches!(project.status, ProjectStatus::Pending | ProjectStatus::Unconfigured);
            let port = if project.is_background || detect_compose_file(project_dir).is_some() {
                None
            } else {
                Some(resolve_app_port(project_dir, port_override)?)
            };
            let url = super::deploy::build_and_deploy(
                client,
                server,
                project_id,
                project_dir,
                port,
                project.is_background,
                secret_override,
                is_first,
                existing_node,
            )
            .await?;
            if let Some(url) = url {
                print_live_url(&url);
            } else if project.is_background {
                print_no_managed_url();
            }
        }
        "Recreate" => {
            println!("  :: Recreating {}...", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/recreate", project_id), &json!({})).await?;
            println!("  {} Recreated", "✔".green());
            println!();
        }
        "Start" => {
            println!("  :: Starting {}...", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/start", project_id), &json!({})).await?;
            println!("  {} Started", "✔".green());
            println!();
        }
        "Stop" => {
            println!("  :: Stopping {}...", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/stop", project_id), &json!({})).await?;
            println!("  {} Stopped", "✔".green());
            println!();
        }
        "Delete" => {
            let confirmed = Confirm::new()
                .with_prompt(format!("Delete project {}? This cannot be undone.", project_id.red()))
                .default(false)
                .interact()?;
            if !confirmed {
                println!("  Cancelled.");
                return Ok(());
            }
            println!("  :: Deleting {}...", project_id.cyan());
            auth::session_delete(client, server, &format!("/projects/{}", project_id)).await?;
            println!("  {} Deleted", "✔".green());
            println!();
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn select_background_project() -> Result<bool> {
    let choices = ["Web app / HTTP API — expose a managed URL", "Background project — no managed URL; stays running"];
    let selection = Select::new().with_prompt("Project type").items(&choices).default(0).interact()?;
    Ok(selection == 1)
}

pub(super) async fn poll_deploy_status(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    success_label: &str,
    fail_label: &str,
) -> Result<()> {
    let final_status = crate::status::poll_project_status(client, server, project_id, 120).await?;

    match final_status.as_ref() {
        Some(ProjectStatus::Running) => {
            println!("  {} {}", "✔".green(), success_label);
            Ok(())
        }
        Some(ProjectStatus::Error) => {
            println!("  {} {}", "✘".red(), fail_label);
            anyhow::bail!("{}", fail_label);
        }
        _ => {
            println!("  {} Deployment is taking longer than expected.", "!".yellow());
            let choices = vec!["Wait", "Detach"];
            let selection =
                Select::new().with_prompt("Continue waiting or detach?").items(&choices).default(0).interact()?;

            if selection == 0 {
                let final_status = crate::status::poll_project_status(client, server, project_id, 300).await?;
                match final_status.as_ref() {
                    Some(ProjectStatus::Running) => {
                        println!("  {} {}", "✔".green(), success_label);
                        Ok(())
                    }
                    Some(ProjectStatus::Error) => {
                        println!("  {} {}", "✘".red(), fail_label);
                        anyhow::bail!("{}", fail_label);
                    }
                    _ => {
                        println!("  Still deploying. Check status with:");
                        println!("    {}", format!("l8b status --project {}", project_id).cyan());
                        Ok(())
                    }
                }
            } else {
                println!("  Detached. Check status with:");
                println!("    {}", format!("l8b status --project {}", project_id).cyan());
                Ok(())
            }
        }
    }
}

pub(super) async fn await_runtime_config_and_start(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    node_id: Option<&str>,
) -> Result<bool> {
    println!();
    println!("  {} {}", "⏸".yellow(), "Awaiting runtime configuration".bold());
    show_env_path(server, project_id, node_id);
    println!("     {}", "Add runtime variables now if needed (DB passwords, API keys, etc.)".dimmed());
    println!(
        "     {}",
        "Select \"Start containers now\" if your compose/app already has defaults or needs no env.".dimmed()
    );

    let choices = vec!["Start containers now", "Pause — start later"];
    let selection = Select::new().with_prompt("Ready to start containers?").items(&choices).default(0).interact()?;

    if selection != 0 {
        println!();
        println!("  {} {}", "!".yellow(), "Paused — containers were not started.".bold());
        println!("     {}", "Your image is ready. Edit the .env above if needed, then run:".dimmed());
        println!("       {}", "l8b ship".cyan());
        println!("     {}", "Select this project and choose \"Resume deployment\".".dimmed());
        return Ok(false);
    }

    let start_spinner = spinner("  🚀 {spinner} {msg}");
    start_spinner.set_message("Starting containers...");

    auth::session_post(client, server, &format!("/projects/{}/start", project_id), &json!({}))
        .await
        .with_context(|| format!("failed to start staged project '{}'", project_id))?;

    start_spinner.set_message("Waiting for deployment...");
    start_spinner.finish_and_clear();

    poll_deploy_status(
        client,
        server,
        project_id,
        "Deploy successful!",
        &format!("Deploy failed for project '{}'", project_id),
    )
    .await?;
    Ok(true)
}

pub(super) async fn select_target_node(
    client: &reqwest::Client,
    server: &str,
    existing: Option<&str>,
) -> Result<Option<String>> {
    if let Some(id) = existing {
        return Ok(Some(id.to_string()));
    }

    let mut nodes = auth::fetch_online_nodes(client, server).await;
    match nodes.len() {
        0 => Ok(None),
        1 => Ok(Some(nodes[0].id.clone())),
        _ => {
            nodes.sort_by(|a, b| {
                let a_rec = a.recommended.unwrap_or(false);
                let b_rec = b.recommended.unwrap_or(false);
                b_rec.cmp(&a_rec)
            });
            let items: Vec<String> = nodes
                .iter()
                .map(|n| {
                    let arch = n.architecture.as_deref().unwrap_or("unknown");
                    let rec = if n.recommended == Some(true) { " [recommended]" } else { "" };
                    format!("  {} ({}){} ", n.name, arch, rec)
                })
                .collect();
            let default_idx = nodes.iter().position(|n| n.recommended == Some(true)).unwrap_or(0);
            let idx = Select::new().with_prompt("Select target node").items(&items).default(default_idx).interact()?;
            Ok(Some(nodes[idx].id.clone()))
        }
    }
}

/// Decide how to upload. An explicit flag always wins; in CI the broker auto-decides;
/// interactively, offer direct upload when the chosen node has a public IP.
pub(super) fn resolve_upload_mode(
    nodes: &[auth::NodeInfo],
    node_id: Option<&str>,
    ci_mode: bool,
    flag: crate::upload::UploadMode,
) -> crate::upload::UploadMode {
    use crate::upload::UploadMode;
    if !matches!(flag, UploadMode::Auto) {
        return flag;
    }
    if ci_mode {
        return UploadMode::Auto;
    }
    // The local node runs on the master itself — every upload path goes to the
    // master, so asking "direct or relay?" is meaningless there.
    if node_id == Some("local") {
        return UploadMode::Auto;
    }
    if let Some(id) = node_id
        && let Some(n) = nodes.iter().find(|n| n.id == id)
        && n.public_ip.as_deref().filter(|s| !s.is_empty()).is_some()
    {
        let items = ["Direct to agent (recommended)", "Relay via master"];
        let idx = Select::new().with_prompt("Upload path").items(&items).default(0).interact().unwrap_or(0);
        return if idx == 0 { UploadMode::Direct } else { UploadMode::Relay };
    }
    UploadMode::Auto
}
