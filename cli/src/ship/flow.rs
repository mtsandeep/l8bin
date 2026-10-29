use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::{Confirm, Input, Select};
use litebin_common::types::{ProjectStatus, TokenScope};
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
    #[serde(default)]
    pub custom_domain: Option<String>,
    #[serde(default)]
    pub public_stats: Option<litebin_common::types::ServiceInfo>,
}

pub async fn run(
    client: &reqwest::Client,
    server: &str,
    path_override: Option<&str>,
    port_override: Option<u16>,
    secret_override: Vec<PathBuf>,
    no_env: bool,
    token_auth: bool,
) -> Result<()> {
    let project_dir = Path::new(path_override.unwrap_or("."));
    let env_source = super::env::EnvSource::from_flags(secret_override, no_env);

    // Project-bound token: one destination, no New/Existing menu.
    if let Some((project_id, scope)) = bound_token(client, server).await {
        return scoped_flow(client, server, project_dir, port_override, &env_source, scope, &project_id).await;
    }

    let projects_json = auth::api_get(client, server, "/projects").await.unwrap_or_else(|e| {
        println!("  {} Could not list projects: {}", "!".yellow(), e);
        serde_json::json!([])
    });
    let projects: Vec<ProjectInfo> = serde_json::from_value(projects_json).unwrap_or_default();

    // A repo already bound by l8b.toml ships to that project by default.
    let stored = crate::project_config::load(project_dir).and_then(|c| c.project);
    let mut choices: Vec<String> = Vec::new();
    let mut stored_idx: Option<usize> = None;
    if let Some(id) = stored.as_deref().filter(|id| projects.iter().any(|p| p.id == *id)) {
        stored_idx = Some(choices.len());
        choices.push(format!("{} (from {})", id.cyan(), crate::project_config::FILE_NAME));
    } else if let Some(id) = &stored {
        println!(
            "  {} {} names project '{}' but it is not on this server.",
            "!".yellow(),
            crate::project_config::FILE_NAME,
            id
        );
    }
    let new_idx = choices.len();
    choices.push("New project".to_string());
    choices.push("Existing project".to_string());
    let selection = Select::new().with_prompt("Deploy to").items(&choices).default(0).interact()?;

    if selection == stored_idx.unwrap_or(usize::MAX) {
        let project = &projects
            [projects.iter().position(|p| stored.as_deref() == Some(p.id.as_str())).expect("offered only when found")];
        choose_action(client, server, project_dir, port_override, &env_source, project, Access::Full).await
    } else if selection == new_idx {
        new_project_flow(client, server, project_dir, port_override, &env_source, token_auth).await
    } else {
        existing_project_flow(client, server, project_dir, &projects, port_override, &env_source).await
    }
}

// ── New / existing project flows ─────────────────────────────────────────────

/// Some((id, scope)) when the credential is a deploy token bound to one project.
async fn bound_token(client: &reqwest::Client, server: &str) -> Option<(String, TokenScope)> {
    let who = auth::api_get(client, server, "/whoami").await.ok()?;
    if who["kind"].as_str() != Some("token") {
        return None;
    }
    let project_id = who["project_id"].as_str()?.to_string();
    let scope = serde_json::from_value(who["scope"].clone()).unwrap_or(TokenScope::Deploy);
    Some((project_id, scope))
}

/// Project-bound token: show the project, then the actions its scope allows.
async fn scoped_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    port_override: Option<u16>,
    env_source: &super::env::EnvSource,
    scope: TokenScope,
    project_id: &str,
) -> Result<()> {
    let json = auth::api_get(client, server, &format!("/projects/{project_id}"))
        .await
        .with_context(|| format!("could not load bound project '{project_id}'"))?;
    let project: ProjectInfo =
        serde_json::from_value(json).with_context(|| format!("unexpected response for project '{project_id}'"))?;

    println!("  {} Token is scoped to project {}:", "::".dimmed(), project_id.cyan());
    println!("{}", project_row(&project));
    choose_action(client, server, project_dir, port_override, env_source, &project, Access::Token { scope }).await
}

/// The Deploy/Redeploy action: build and deploy the project's image.
async fn deploy_existing(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    port_override: Option<u16>,
    env_source: &super::env::EnvSource,
    project: &ProjectInfo,
) -> Result<()> {
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
        &project.id,
        project_dir,
        port,
        project.is_background,
        env_source.clone(),
        is_first,
        existing_node,
    )
    .await?;
    if let Some(url) = url {
        print_live_url(&url);
    } else if project.is_background {
        print_no_managed_url();
    }
    Ok(())
}

/// The Resume-deployment action: env setup, then start containers.
async fn resume_existing(client: &reqwest::Client, server: &str, project: &ProjectInfo) -> Result<()> {
    let started = await_runtime_config_and_start(client, server, &project.id, project.node_id.as_deref()).await?;
    if started {
        if project.is_background {
            print_no_managed_url();
        } else {
            let live_url = match project.custom_domain.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                Some(d) => format!("https://{d}"),
                None => auth::project_live_url(&project.id, &auth::fetch_platform_domain(client, server).await),
            };
            print_live_url(&live_url);
        }
    }
    Ok(())
}

async fn new_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    port_override: Option<u16>,
    env_source: &super::env::EnvSource,
    token_auth: bool,
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

    let mut minted_token: Option<String> = None;
    if token_auth {
        // Project creation and token minting are session-only; the deploy
        // stages the project itself, and CI tokens come from the dashboard.
        println!("  {} Token auth: the deploy will create project {}.", "::".dimmed(), name.cyan());
        println!("  {} For a CI token, add one under Settings {} Access Tokens.", "::".dimmed(), "→".cyan());
        println!();
    } else {
        println!("  :: Creating project {}...", name.cyan());
        auth::session_post(client, server, "/projects", &json!({"id": &name, "is_background": is_background}))
            .await
            .with_context(|| format!("failed to create project '{}'", name))?;
        println!("  {} Project created", "✔".green());

        println!("  :: Generating deploy token for {}...", name.cyan());
        let token_resp = auth::session_post(
            client,
            server,
            "/deploy-tokens",
            &json!({"project_id": &name, "name": "cli-generated"}),
        )
        .await?;

        let token = token_resp["token"].as_str().unwrap_or("<error>").to_string();
        minted_token = Some(token.clone());

        println!();
        println!("  {} Deploy token generated for {}", "🔐".dimmed(), name.cyan());
        println!("  {} Save it for CI/CD:", "!".yellow());
        println!("  {}", format!("L8B_TOKEN={}", token).dimmed());
        println!();
    }

    let port = if is_background { None } else { Some(resolve_app_port(project_dir, port_override)?) };

    let url = super::deploy::build_and_deploy(
        client,
        server,
        &name,
        project_dir,
        port,
        is_background,
        env_source.clone(),
        true,
        None,
    )
    .await?;

    if let Some(url) = url {
        print_live_url(&url);
    } else {
        print_no_managed_url();
    }
    if let Some(token) = minted_token {
        println!("  {} Use this token to redeploy from CI/CD:", "💡".dimmed());
        println!("  {}", format!("L8B_TOKEN={token} l8b deploy --project {name}").dimmed());
    }
    println!();

    Ok(())
}

async fn existing_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    projects: &[ProjectInfo],
    port_override: Option<u16>,
    env_source: &super::env::EnvSource,
) -> Result<()> {
    if projects.is_empty() {
        anyhow::bail!("No existing projects found. Create one with the 'New project' option.");
    }

    let mut projects: Vec<&ProjectInfo> = projects.iter().collect();
    projects.sort_by(|a, b| a.id.cmp(&b.id));

    let items: Vec<String> = projects.iter().map(|p| project_row(p)).collect();

    let idx = Select::new().with_prompt("Select project").items(&items).interact()?;

    choose_action(client, server, project_dir, port_override, env_source, projects[idx], Access::Full).await
}

/// One project row as shown in the select list.
fn project_row(p: &ProjectInfo) -> String {
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
        p.public_stats.as_ref().and_then(|ps| ps.mapped_port).map(|p| format!("port {}", p)).unwrap_or("—".to_string())
    };
    format!("  {:<20} {:<12} {:<25} {}", p.id, status, image, port)
}

/// Which actions a credential may take on a project.
enum Access {
    /// Session/global: every action.
    Full,
    /// Token: lifecycle actions at manage scope, Delete at admin scope.
    Token { scope: TokenScope },
}

/// Action menu for a chosen project; `access` decides which actions are offered.
async fn choose_action(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    port_override: Option<u16>,
    env_source: &super::env::EnvSource,
    project: &ProjectInfo,
    access: Access,
) -> Result<()> {
    let (manage, admin) = match access {
        Access::Full => (true, true),
        Access::Token { scope } => (scope >= TokenScope::Manage, scope >= TokenScope::Admin),
    };
    let project_id = &project.id;

    let staged = project.status == ProjectStatus::Unconfigured && project_is_staged(project);
    let awaiting_first_deploy =
        project.status == ProjectStatus::Pending || (project.status == ProjectStatus::Unconfigured && !staged);
    let mut actions: Vec<&str> = if staged {
        vec!["Resume deployment", "Redeploy"]
    } else if awaiting_first_deploy {
        vec!["Deploy"]
    } else {
        vec!["Redeploy"]
    };
    if !staged && !awaiting_first_deploy && manage {
        actions.extend(["Recreate", "Start", "Stop"]);
    }
    if admin {
        actions.push("Delete");
    }
    let action_idx = Select::new().with_prompt("Action").items(&actions).default(0).interact()?;

    match actions[action_idx] {
        "Resume deployment" => resume_existing(client, server, project).await?,
        "Deploy" | "Redeploy" => {
            deploy_existing(client, server, project_dir, port_override, env_source, project).await?
        }
        "Recreate" => {
            println!("  :: Recreating {}...", project_id.cyan());
            auth::api_post_json(client, server, &format!("/projects/{}/recreate", project_id), &json!({})).await?;
            println!("  {} Recreated", "✔".green());
            println!();
        }
        "Start" => {
            println!("  :: Starting {}...", project_id.cyan());
            auth::api_post_json(client, server, &format!("/projects/{}/start", project_id), &json!({})).await?;
            println!("  {} Started", "✔".green());
            println!();
        }
        "Stop" => {
            println!("  :: Stopping {}...", project_id.cyan());
            auth::api_post_json(client, server, &format!("/projects/{}/stop", project_id), &json!({})).await?;
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
            auth::api_delete(client, server, &format!("/projects/{}", project_id)).await?;
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
    let final_status = crate::status::poll_project_status(client, server, project_id, 120, false).await?;

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
                let final_status = crate::status::poll_project_status(client, server, project_id, 300, false).await?;
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
    println!(
        "     {}",
        "Runtime variables (DB passwords, API keys, etc.) are injected when containers start.".dimmed()
    );

    // Best-effort: env already on the server (e.g. a resume) skips the setup choice.
    let mut has_env = print_server_env(client, server, project_id).await;
    let mut manual = false;

    if !has_env {
        let setup_choices = vec![
            "Push from a local .env — LiteBin writes it for me",
            "I'll place the .env on the server myself",
            "Skip — this app needs no runtime env",
        ];
        let selection =
            Select::new().with_prompt("  🔒 Runtime env setup").items(&setup_choices).default(0).interact()?;

        match selection {
            0 => {
                push_runtime_env_file(client, server, project_id).await?;
                has_env = print_server_env(client, server, project_id).await;
            }
            1 => {
                manual = true;
                println!();
                println!("  {} A placeholder env file is already on the node — edit it and come back:", "🔒".dimmed());
                show_env_path(server, project_id, node_id);
                println!("     {}", "Your changes are picked up when the containers start.".dimmed());
                println!();
            }
            _ => {}
        }
    }

    loop {
        let push_label = if has_env { "Push another env file" } else { "Push a local .env file" };
        let mut choices = vec!["Start containers now".to_string(), push_label.to_string()];
        if manual {
            choices.push("Check what's on the server".to_string());
        }
        choices.push("Pause — start later".to_string());
        let selection = Select::new().with_prompt("Runtime configuration").items(&choices).default(0).interact()?;

        match selection {
            0 => break,
            1 => {
                push_runtime_env_file(client, server, project_id).await?;
                has_env = print_server_env(client, server, project_id).await;
            }
            2 if manual => {
                if !print_server_env(client, server, project_id).await {
                    println!("  {} {}", "!".yellow(), "No variables on the server yet.".dimmed());
                }
            }
            _ => {
                println!();
                println!("  {} {}", "!".yellow(), "Paused — containers were not started.".bold());
                println!("     {}", "Push env later with `l8b env push`, or edit the file on the node:".dimmed());
                show_env_path(server, project_id, node_id);
                println!("     {}", "Then run `l8b ship` and choose \"Resume deployment\".".dimmed());
                return Ok(false);
            }
        }
    }

    let start_spinner = spinner("  🚀 {spinner} {msg}");
    start_spinner.set_message("Starting containers...");

    auth::api_post_json(client, server, &format!("/projects/{}/start", project_id), &json!({}))
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

/// Redeploy runtime-env checkpoint: keep current (default) or push from a
/// local file. Prints nothing unless you push.
pub(super) async fn maybe_update_runtime_env(client: &reqwest::Client, server: &str, project_id: &str) -> Result<()> {
    let choices = vec!["Skip, no changes to env", "Push from a local .env file"];
    let selection = Select::new().with_prompt("  🔒 Runtime env").items(&choices).default(0).interact()?;
    if selection == 1 {
        push_runtime_env_file(client, server, project_id).await?;
    }
    Ok(())
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

/// Print the project's server-side runtime env (masked previews). Returns
/// whether any variables are set. Best-effort — failures print nothing.
async fn print_server_env(client: &reqwest::Client, server: &str, project_id: &str) -> bool {
    let Ok(env) = auth::api_get(client, server, &format!("/projects/{project_id}/env")).await else {
        return false;
    };
    let vars = env["vars"].as_array().cloned().unwrap_or_default();
    if vars.is_empty() {
        return false;
    }
    println!("  {} Runtime env on server — {} variable(s):", "⚙".dimmed(), vars.len());
    for v in &vars {
        let key = v["key"].as_str().unwrap_or("?");
        let masked = v["masked"].as_str().unwrap_or("");
        let shown = if masked.is_empty() { "(empty)".dimmed().to_string() } else { masked.yellow().to_string() };
        println!("      {} = {}", key.cyan(), shown);
    }
    println!();
    true
}

/// Pick a local `.env` file (or enter a path) and push it as the project's
/// runtime env (merge). Values are write-only; applies on container start.
async fn push_runtime_env_file(client: &reqwest::Client, server: &str, project_id: &str) -> Result<()> {
    let discovered = super::env::discover_env_files(std::path::Path::new("."), false)?;
    let file: String = if discovered.is_empty() {
        dialoguer::Input::new().with_prompt("Path to env file").interact_text()?
    } else {
        let mut items = discovered.clone();
        items.push("(enter a path manually)".to_string());
        let idx = Select::new().with_prompt("Push which file?").items(&items).default(0).interact()?;
        if idx == discovered.len() {
            dialoguer::Input::new().with_prompt("Path to env file").interact_text()?
        } else {
            discovered[idx].clone()
        }
    };

    let raw = std::fs::read(&file).with_context(|| format!("failed to read {file}"))?;
    let vars: std::collections::HashMap<String, String> = dotenvy::Iter::new(raw.as_slice())
        .filter_map(|item| item.ok())
        .map(|(k, v)| (k.trim().to_string(), v))
        .collect();
    if vars.is_empty() {
        anyhow::bail!("no environment variables found in {file}");
    }

    let resp = crate::auth::api_put_json(
        client,
        server,
        &format!("/projects/{project_id}/env"),
        &serde_json::json!({ "env": vars, "mode": "merge" }),
    )
    .await?;
    let keys = resp["vars"].as_array().map(Vec::len).unwrap_or(0);
    println!("  {} Pushed {keys} variable(s) from {file}.", "✓".green());
    println!("  {}", "They take effect when the containers start.".dimmed());
    Ok(())
}
