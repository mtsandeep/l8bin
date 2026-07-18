use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
use litebin_common::types::{COMPOSE_FILE_NAMES, ProjectStatus};
use serde_json::json;

use crate::auth;

#[derive(serde::Deserialize)]
struct PublicStats {
    image: Option<String>,
    #[allow(dead_code)]
    port: Option<u16>,
    mapped_port: Option<u16>,
}

#[derive(serde::Deserialize)]
struct ProjectInfo {
    id: String,
    status: ProjectStatus,
    node_id: Option<String>,
    #[serde(default)]
    is_background: bool,
    #[serde(default)]
    is_staged: bool,
    public_stats: Option<PublicStats>,
}

#[derive(Clone)]
struct BuildInfo {
    svc_name: String,
    build_context: String,
    dockerfile: Option<String>,
}

/// Options for non-interactive compose deploy (used by `deploy` command).
pub struct ComposeDeployOpts {
    /// If Some, only build these services (no interactive prompt).
    pub target_services: Option<Vec<String>>,
    /// Target node ID (optional).
    pub node_id: Option<String>,
    /// Capability ids to grant (e.g. docker-access, raw-ports).
    pub grant_capabilities: Vec<String>,
    /// Deploy the whole project without managed HTTP ingress.
    pub is_background: bool,
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
    let selection = Select::new()
        .with_prompt("Deploy to")
        .items(&choices)
        .default(0)
        .interact()?;

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
            if !input
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err("Use only lowercase letters, numbers, and hyphens");
            }
            Ok(())
        })
        .interact_text()?;

    let is_background = select_background_project()?;

    println!("  {} Creating project {}...", "::", name.cyan());
    auth::session_post(
        client,
        server,
        "/projects",
        &json!({"id": &name, "is_background": is_background}),
    )
        .await
        .with_context(|| format!("failed to create project '{}'", name))?;
    println!("  {} Project created", "✔".green());

    println!("  {} Generating deploy token for {}...", "::", name.cyan());
    let token_resp = auth::session_post(
        client,
        server,
        "/deploy-tokens",
        &json!({"project_id": &name, "name": "cli-generated"}),
    )
    .await?;

    let token = token_resp["token"]
        .as_str()
        .unwrap_or("<error>")
        .to_string();

    println!();
    println!(
        "  {} Deploy token generated for {}",
        "🔐".dimmed(),
        name.cyan()
    );
    println!("  {} Save it for CI/CD:", "!".yellow());
    println!("  {}", format!("L8B_TOKEN={}", token).dimmed());
    println!();

    let port = if is_background {
        None
    } else {
        Some(resolve_app_port(project_dir, port_override)?)
    };

    let url = build_and_deploy(
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
        format!(
            "L8B_TOKEN={} l8b deploy --project {} --background",
            token, name
        )
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
                ProjectStatus::Pending | ProjectStatus::Stopped | ProjectStatus::Unconfigured => p.status.to_string().yellow().to_string(),
                ProjectStatus::Error | ProjectStatus::Degraded => p.status.to_string().red().to_string(),
                _ => p.status.to_string(),
            };
            let image = p
                .public_stats
                .as_ref()
                .and_then(|ps| ps.image.as_deref())
                .map(short_image)
                .unwrap_or_else(|| "—".to_string());
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

    let idx = Select::new()
        .with_prompt("Select project")
        .items(&items)
        .interact()?;

    let project = &projects[idx];
    let project_id = &project.id;

    let staged = project.status == ProjectStatus::Unconfigured && project_is_staged(project);
    let awaiting_first_deploy = project.status == ProjectStatus::Pending
        || (project.status == ProjectStatus::Unconfigured && !staged);
    let actions: Vec<&str> = if staged {
        vec!["Resume deployment", "Redeploy", "Delete"]
    } else if awaiting_first_deploy {
        vec!["Deploy", "Delete"]
    } else {
        vec!["Redeploy", "Recreate", "Start", "Stop", "Delete"]
    };
    let action_idx = Select::new()
        .with_prompt("Action")
        .items(&actions)
        .default(0)
        .interact()?;

    match actions[action_idx] {
        "Resume deployment" => {
            let started = await_runtime_config_and_start(
                client,
                server,
                project_id,
                project.node_id.as_deref(),
            )
            .await?;
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
            let url = build_and_deploy(
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
            println!("  {} Recreating {}...", "::", project_id.cyan());
            auth::session_post(
                client,
                server,
                &format!("/projects/{}/recreate", project_id),
                &json!({}),
            )
            .await?;
            println!("  {} Recreated", "✔".green());
            println!();
        }
        "Start" => {
            println!("  {} Starting {}...", "::", project_id.cyan());
            auth::session_post(
                client,
                server,
                &format!("/projects/{}/start", project_id),
                &json!({}),
            )
            .await?;
            println!("  {} Started", "✔".green());
            println!();
        }
        "Stop" => {
            println!("  {} Stopping {}...", "::", project_id.cyan());
            auth::session_post(
                client,
                server,
                &format!("/projects/{}/stop", project_id),
                &json!({}),
            )
            .await?;
            println!("  {} Stopped", "✔".green());
            println!();
        }
        "Delete" => {
            let confirmed = Confirm::new()
                .with_prompt(format!(
                    "Delete project {}? This cannot be undone.",
                    project_id.red()
                ))
                .default(false)
                .interact()?;
            if !confirmed {
                println!("  Cancelled.");
                return Ok(());
            }
            println!("  {} Deleting {}...", "::", project_id.cyan());
            auth::session_delete(client, server, &format!("/projects/{}", project_id)).await?;
            println!("  {} Deleted", "✔".green());
            println!();
        }
        _ => unreachable!(),
    }

    Ok(())
}

// ── Shared helpers ───────────────────────────────────────────────────────────

fn resolve_app_port(project_dir: &Path, port_override: Option<u16>) -> Result<u16> {
    if let Some(p) = port_override {
        return Ok(p);
    }
    if detect_compose_file(project_dir).is_some() {
        return Ok(0);
    }

    let detected = detect_exposed_ports(project_dir);
    match detected.as_slice() {
        [single] => {
            println!("  {} Detected exposed port {}", "::".dimmed(), single);
            Ok(*single)
        }
        [first, rest @ ..] => {
            let all: Vec<String> = std::iter::once(first)
                .chain(rest.iter())
                .map(|p| p.to_string())
                .collect();
            println!(
                "  {} Detected ports: {} — using {}",
                "::".dimmed(),
                all.join(", "),
                first
            );
            Ok(*first)
        }
        [] => {
            let input: String = Input::new()
                .with_prompt("App port")
                .default("3000".to_string())
                .interact_text()?;
            input.parse::<u16>().context("Port must be a number (1-65535)")
        }
    }
}

fn detect_exposed_ports(project_dir: &Path) -> Vec<u16> {
    let mut ports = Vec::new();

    let dockerfile = project_dir.join("Dockerfile");
    if let Ok(content) = std::fs::read_to_string(&dockerfile) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.to_uppercase().starts_with("EXPOSE") {
                for part in trimmed.split_whitespace().skip(1) {
                    let port_part = part.split('/').next().unwrap_or(part);
                    if let Ok(p) = port_part.parse::<u16>() {
                        if !ports.contains(&p) {
                            ports.push(p);
                        }
                    }
                }
            }
        }
    }

    ports
}

fn parse_container_port(port_str: &str) -> Option<u16> {
    let port_part = port_str.split('/').next().unwrap_or(port_str);
    let container_port = port_part.rsplit(':').next().unwrap_or(port_part);
    container_port.parse().ok()
}

fn service_has_public_label(svc: &serde_yaml::Value) -> bool {
    match svc.get("labels") {
        Some(serde_yaml::Value::Mapping(m)) => m.keys().any(|k| {
            k.as_str()
                .map(|k| k == "litebin.public" || k.ends_with(".public"))
                .unwrap_or(false)
        }),
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .any(|v| v.as_str().map(|s| s.contains("litebin.public")).unwrap_or(false)),
        _ => false,
    }
}

fn public_service_candidates(compose: &serde_yaml::Value) -> (Vec<(String, u16)>, bool, bool) {
    let Some(services) = compose.get("services").and_then(|s| s.as_mapping()) else {
        return (Vec::new(), false, false);
    };

    let mut has_public_label = false;
    let mut candidates: Vec<(String, u16)> = Vec::new();
    let mut has_well_known = false;

    for (svc_name, svc) in services {
        if service_has_public_label(svc) {
            has_public_label = true;
        }
        if let Some(port_list) = svc.get("ports").and_then(|p| p.as_sequence()) {
            for port_val in port_list {
                if let Some(port_str) = port_val.as_str() {
                    if let Some(p) = parse_container_port(port_str) {
                        if p == 80 || p == 443 {
                            has_well_known = true;
                        }
                        if !candidates.iter().any(|(_, ep)| *ep == p) {
                            candidates.push((
                                svc_name.as_str().unwrap_or_default().to_string(),
                                p,
                            ));
                        }
                    }
                }
            }
        }
    }

    (candidates, has_well_known, has_public_label)
}

/// Interactive public-service picker. Returns Some(name) when a label must be injected.
fn pick_public_service(compose: &serde_yaml::Value) -> Result<Option<String>> {
    let (candidates, has_well_known, has_public_label) = public_service_candidates(compose);
    if has_public_label || has_well_known || candidates.len() <= 1 {
        return Ok(None);
    }

    let items: Vec<String> = candidates
        .iter()
        .map(|(name, port)| format!("{} (port {})", name, port))
        .collect();

    println!(
        "  {} Multiple services expose ports — select the public service",
        "!".yellow()
    );
    let selection = Select::new()
        .with_prompt("Public service (main subdomain entry point)")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(Some(candidates[selection].0.clone()))
}

/// Non-interactive: auto-pick first candidate when ambiguous.
fn auto_pick_public_service(compose: &serde_yaml::Value) -> Option<String> {
    let (candidates, has_well_known, has_public_label) = public_service_candidates(compose);
    if has_public_label || has_well_known || candidates.len() <= 1 {
        return None;
    }

    let (name, port) = &candidates[0];
    println!(
        "  {} Multiple services expose ports — auto-selecting {} (port {}) as public",
        "::".dimmed(),
        name.cyan(),
        port
    );
    Some(name.clone())
}

fn inject_public_label(yaml: &str, service_name: &str) -> Result<String> {
    let mut doc: serde_yaml::Value = serde_yaml::from_str(yaml)
        .with_context(|| "failed to parse compose YAML for label injection")?;

    if let Some(services) = doc.get_mut("services").and_then(|s| s.as_mapping_mut()) {
        if let Some(svc) = services.get_mut(&serde_yaml::Value::String(service_name.to_string())) {
            if let Some(svc_map) = svc.as_mapping_mut() {
                let labels = svc_map
                    .entry(serde_yaml::Value::String("labels".to_string()))
                    .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                if let Some(labels_map) = labels.as_mapping_mut() {
                    labels_map.insert(
                        serde_yaml::Value::String("litebin.public".to_string()),
                        serde_yaml::Value::String("true".to_string()),
                    );
                }
            }
        }
    }

    serde_yaml::to_string(&doc)
        .with_context(|| "failed to serialize compose YAML after label injection")
}

fn env_precedence_score(name: &str) -> i32 {
    if name == ".env.example" {
        0
    } else if name == ".env" {
        1
    } else if name.contains(".local") || name.contains(".prod") {
        3
    } else {
        2
    }
}

fn discover_env_files(dir: &Path, exclude_example: bool) -> Result<Vec<String>> {
    let mut env_files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".env"))
        .filter(|name| !(exclude_example && name == ".env.example"))
        .collect();

    env_files.sort_by(|a, b| {
        env_precedence_score(a)
            .cmp(&env_precedence_score(b))
            .then(a.cmp(b))
    });
    Ok(env_files)
}

enum EnvSelectMode {
    Interactive,
    InteractiveNoCustomOrder,
    AutoAllExceptExample,
}

fn select_env_files(project_dir: &Path, mode: EnvSelectMode) -> Result<Vec<PathBuf>> {
    match mode {
        EnvSelectMode::AutoAllExceptExample => {
            let files = discover_env_files(project_dir, true)?;
            if !files.is_empty() {
                println!(
                    "  {} Using .env files: {}",
                    "::".dimmed(),
                    files.join(", ").dimmed()
                );
            }
            Ok(files.into_iter().map(|n| project_dir.join(n)).collect())
        }
        EnvSelectMode::Interactive | EnvSelectMode::InteractiveNoCustomOrder => {
            let env_files = discover_env_files(project_dir, false)?;
            if env_files.is_empty() {
                return Ok(Vec::new());
            }

            let allow_custom = matches!(mode, EnvSelectMode::Interactive);
            loop {
                let mut choices = vec![
                    "Yes (all / standard order)".to_string(),
                    "No".to_string(),
                    "Pick specific...".to_string(),
                ];
                if allow_custom {
                    choices.push("Custom order (manual input)".to_string());
                }

                let selection = Select::new()
                    .with_prompt("  🔒 Found .env files. Include build-time secrets?")
                    .items(&choices)
                    .default(0)
                    .interact()?;

                match selection {
                    0 => {
                        println!(
                            "  {} Using standard merge order (later files override earlier ones):",
                            "::".dimmed()
                        );
                        println!("     {}", env_files.join(" < ").dimmed());
                        return Ok(env_files.iter().map(|n| project_dir.join(n)).collect());
                    }
                    1 => {
                        println!("  {} No build-time secrets included", "::".dimmed());
                        return Ok(Vec::new());
                    }
                    2 => {
                        let chosen = MultiSelect::new()
                            .with_prompt(
                                "  🔒 Select secrets (Standard merge order applies) [Space to select, Enter to confirm]",
                            )
                            .items(&env_files)
                            .interact()?;

                        if chosen.is_empty() {
                            println!(
                                "  {} {}",
                                "!".red(),
                                "No files selected. Pick at least one, or choose 'No' to continue without secrets."
                                    .yellow()
                            );
                            continue;
                        }

                        let selected: Vec<&str> = chosen.iter().map(|&i| env_files[i].as_str()).collect();
                        println!("  {} Merging: {}", "::".dimmed(), selected.join(" < ").dimmed());
                        return Ok(chosen.into_iter().map(|i| project_dir.join(&env_files[i])).collect());
                    }
                    3 if allow_custom => {
                        println!(
                            "  {} Available files: {}",
                            "::".dimmed(),
                            env_files.join(", ").dimmed()
                        );
                        let input: String = Input::new()
                            .with_prompt(
                                "  🔒 Enter filenames in merge order (space separated, e.g. .env .env.local)",
                            )
                            .interact_text()?;

                        let parts: Vec<&str> = input.split_whitespace().collect();
                        let mut selected = Vec::new();
                        for part in &parts {
                            let path = project_dir.join(part);
                            if path.exists() {
                                selected.push(path);
                            } else {
                                println!("  {} {} does not exist, skipping", "!".yellow(), part);
                            }
                        }
                        if selected.is_empty() {
                            println!("  {} {}", "!".red(), "No valid files entered.".yellow());
                            continue;
                        }
                        println!(
                            "  {} Merging in your exact order: {}",
                            "::".dimmed(),
                            parts.join(" < ").dimmed()
                        );
                        return Ok(selected);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

fn merge_service_env_files(root_env: &[PathBuf], svc_dir: &Path) -> Vec<PathBuf> {
    let mut secret = root_env.to_vec();
    if let Ok(entries) = std::fs::read_dir(svc_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".env")
                && !secret
                    .iter()
                    .any(|p| p.file_name() == Some(entry.file_name().as_os_str()))
            {
                secret.push(entry.path());
            }
        }
    }
    secret
}

fn spinner(template: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template(template).unwrap());
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb
}

fn stop_buildkit() {
    println!("  🧹 Stopping BuildKit...");
    let _ = std::process::Command::new("docker")
        .args(["stop", "buildkit"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

async fn resolve_live_url(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    api_url: Option<&str>,
) -> String {
    if let Some(url) = api_url {
        let url = url.trim();
        if !url.is_empty() && !url.contains("https://https://") && !url.contains(".https://") {
            if url.starts_with("http://") || url.starts_with("https://") {
                return url.to_string();
            }
            return format!("https://{}", url);
        }
    }
    let domain = auth::fetch_platform_domain(client, server).await;
    auth::project_live_url(project_id, &domain)
}

fn show_env_path(server: &str, project_id: &str, node_id: Option<&str>) {
    let is_local = server.contains("localhost") || server.contains("127.0.0.1");
    let home_prefix = if is_local {
        dirs::home_dir()
            .map(|h| format!("{}{sep}litebin", h.display(), sep = std::path::MAIN_SEPARATOR))
            .unwrap_or_else(|| "~/litebin".to_string())
    } else {
        "~/litebin".to_string()
    };
    let sep = std::path::MAIN_SEPARATOR;
    let home_env = format!("{}{sep}projects{sep}{project_id}{sep}.env", home_prefix);
    let rel_env = format!(".{sep}litebin{sep}projects{sep}{project_id}{sep}.env");
    let node_label = node_id.unwrap_or("local");
    println!(
        "  {} Runtime secrets on node {}: {}  or  {}",
        "🔒".dimmed(),
        node_label.cyan(),
        home_env.yellow(),
        rel_env.yellow()
    );
    println!(
        "     {}",
        "(default install path; if custom -InstallDir was used, prepend that path instead)".dimmed()
    );
    if node_id.is_some() && node_id != Some("local") {
        println!(
            "     {}",
            "Edit this file on the selected agent node before continuing.".dimmed()
        );
    }
}

fn project_is_staged(project: &ProjectInfo) -> bool {
    project.is_staged
        || project
            .public_stats
            .as_ref()
            .and_then(|ps| ps.image.as_ref())
            .map(|img| !img.is_empty())
            .unwrap_or(false)
}

fn short_image(image: &str) -> String {
    let hash = image.strip_prefix("sha256:").unwrap_or(image);
    if hash.len() > 12 {
        hash[..12].to_string()
    } else {
        hash.to_string()
    }
}

fn print_live_url(url: &str) {
    println!();
    println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
    println!();
}

fn print_no_managed_url() {
    println!();
    println!("  {} No managed URL (background project)", "⚙".dimmed());
    println!();
}

fn select_background_project() -> Result<bool> {
    let choices = [
        "Web app / HTTP API — expose a managed URL",
        "Background project — no managed URL; stays running",
    ];
    let selection = Select::new()
        .with_prompt("Project type")
        .items(&choices)
        .default(0)
        .interact()?;
    Ok(selection == 1)
}

/// Detect compose file in the given directory. Returns the filename or None.
pub fn detect_compose_file(project_dir: &Path) -> Option<&'static str> {
    COMPOSE_FILE_NAMES
        .iter()
        .find(|p| project_dir.join(p).exists())
        .copied()
}

/// Resolve the target Docker platform string from a list of nodes.
pub fn resolve_platform(nodes: &[crate::auth::NodeInfo], node_id: Option<&str>) -> Option<String> {
    let arch = match node_id {
        Some(id) => nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.architecture.as_deref()),
        None => nodes
            .iter()
            .find(|n| n.recommended == Some(true))
            .or_else(|| nodes.first())
            .and_then(|n| n.architecture.as_deref()),
    };
    arch.map(|a| match a {
        "aarch64" => "linux/arm64".to_string(),
        "x86_64" => "linux/amd64".to_string(),
        other => format!("linux/{}", other),
    })
}

async fn poll_deploy_status(
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
            println!(
                "  {} Deployment is taking longer than expected.",
                "!".yellow()
            );
            let choices = vec!["Wait", "Detach"];
            let selection = Select::new()
                .with_prompt("Continue waiting or detach?")
                .items(&choices)
                .default(0)
                .interact()?;

            if selection == 0 {
                let final_status =
                    crate::status::poll_project_status(client, server, project_id, 300).await?;
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
                        println!(
                            "    {}",
                            format!("l8b status --project {}", project_id).cyan()
                        );
                        Ok(())
                    }
                }
            } else {
                println!("  Detached. Check status with:");
                println!(
                    "    {}",
                    format!("l8b status --project {}", project_id).cyan()
                );
                Ok(())
            }
        }
    }
}

async fn await_runtime_config_and_start(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    node_id: Option<&str>,
) -> Result<bool> {
    println!();
    println!(
        "  {} {}",
        "⏸".yellow(),
        "Awaiting runtime configuration".bold()
    );
    show_env_path(server, project_id, node_id);
    println!(
        "     {}",
        "Add runtime variables now if needed (DB passwords, API keys, etc.).".dimmed()
    );
    println!(
        "     {}",
        "Select \"Start containers now\" if your compose/app already has defaults or needs no env."
            .dimmed()
    );

    let choices = vec!["Start containers now", "Pause — start later"];
    let selection = Select::new()
        .with_prompt("Ready to start containers?")
        .items(&choices)
        .default(0)
        .interact()?;

    if selection != 0 {
        println!();
        println!(
            "  {} {}",
            "!".yellow(),
            "Paused — containers were not started.".bold()
        );
        println!(
            "     {}",
            "Your image is ready. Edit the .env above if needed, then run:".dimmed()
        );
        println!("       {}", "l8b ship".cyan());
        println!(
            "     {}",
            "Select this project and choose \"Resume deployment\".".dimmed()
        );
        return Ok(false);
    }

    let start_spinner = spinner("  🚀 {spinner} {msg}");
    start_spinner.set_message("Starting containers...");

    auth::session_post(
        client,
        server,
        &format!("/projects/{}/start", project_id),
        &json!({}),
    )
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

async fn select_target_node(
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
                    let rec = if n.recommended == Some(true) {
                        " [recommended]"
                    } else {
                        ""
                    };
                    format!("  {} ({}){} ", n.name, arch, rec)
                })
                .collect();
            let default_idx = nodes
                .iter()
                .position(|n| n.recommended == Some(true))
                .unwrap_or(0);
            let idx = Select::new()
                .with_prompt("Select target node")
                .items(&items)
                .default(default_idx)
                .interact()?;
            Ok(Some(nodes[idx].id.clone()))
        }
    }
}

fn collect_build_infos(compose: &serde_yaml::Value) -> Vec<BuildInfo> {
    let mut build_infos = Vec::new();
    let Some(services) = compose.get("services").and_then(|s| s.as_mapping()) else {
        return build_infos;
    };

    for (svc_name, svc_config) in services {
        if svc_config.get("build").is_none() || svc_config.get("image").is_some() {
            continue;
        }
        let name = svc_name.as_str().unwrap_or_default().to_string();
        let (ctx, dockerfile) = match svc_config.get("build") {
            Some(b) if b.as_str().is_some() => (b.as_str().unwrap_or(&name).to_string(), None),
            Some(b) if b.as_mapping().is_some() => {
                let build_map = b.as_mapping().unwrap();
                let context = build_map
                    .get("context")
                    .and_then(|c| c.as_str())
                    .unwrap_or(&name)
                    .to_string();
                let df = build_map
                    .get("dockerfile")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());
                (context, df)
            }
            _ => (name.clone(), None),
        };
        build_infos.push(BuildInfo {
            svc_name: name,
            build_context: ctx,
            dockerfile,
        });
    }
    build_infos
}

fn rewrite_compose_images(
    compose: &serde_yaml::Value,
    resolved_images: &std::collections::HashMap<String, String>,
) -> Result<String> {
    let mut resolved_compose = compose.clone();
    if let Some(services_map) = resolved_compose
        .get_mut("services")
        .and_then(|s| s.as_mapping_mut())
    {
        for entry in services_map.iter_mut() {
            let svc_name = entry.0.as_str().unwrap_or_default().to_string();
            if let Some(image_id) = resolved_images.get(&svc_name) {
                if let Some(svc_map) = entry.1.as_mapping_mut() {
                    svc_map.remove("build");
                    svc_map.insert(
                        serde_yaml::Value::String("image".to_string()),
                        serde_yaml::Value::String(image_id.clone()),
                    );
                }
            }
        }
    }
    Ok(serde_yaml::to_string(&resolved_compose)?)
}

async fn build_and_upload_services(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    build_infos: &[BuildInfo],
    root_env_paths: &[PathBuf],
    node_id: Option<&str>,
    platform: Option<&str>,
    ci_mode: bool,
) -> Result<std::collections::HashMap<String, String>> {
    let mut resolved_images = std::collections::HashMap::new();

    for info in build_infos {
        let svc_dir = project_dir
            .join(&info.build_context)
            .canonicalize()
            .with_context(|| {
                format!(
                    "build context '{}' does not exist for service '{}'",
                    info.build_context, info.svc_name
                )
            })?;
        let svc_dir = Path::new(svc_dir.to_str().unwrap().trim_start_matches(r"\\?"));

        let secret = merge_service_env_files(root_env_paths, svc_dir);
        let image_tag = format!(
            "{}/{}-{}",
            crate::config::IMAGE_PREFIX,
            project_id,
            info.svc_name
        );
        println!(
            "    {} {} ({})",
            "→".dimmed(),
            info.svc_name.cyan(),
            svc_dir.display()
        );

        let saved_image = crate::build::build_project(
            svc_dir,
            info.dockerfile.as_deref(),
            &image_tag,
            secret,
            ci_mode,
            platform,
        )
        .await?;
        println!(
            "    {} {} — {}",
            "  ✓".green(),
            info.svc_name,
            HumanBytes(saved_image.compressed_size)
        );

        let image_id = crate::upload::upload_tar(
            client,
            server,
            project_id,
            Path::new(&saved_image.path),
            &saved_image.image_id,
            node_id,
            ci_mode,
        )
        .await?;

        resolved_images.insert(info.svc_name.clone(), image_id);
        let _ = std::fs::remove_file(&saved_image.path);
    }

    Ok(resolved_images)
}

async fn submit_compose(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    compose_name: &str,
    resolved_yaml: String,
    is_partial_build: bool,
    build_infos: &[BuildInfo],
    node_id: Option<&str>,
    stage_only: bool,
    is_background: bool,
    grant_capabilities: &[String],
) -> Result<serde_json::Value> {
    let mut form = reqwest::multipart::Form::new()
        .text("project_id", project_id.to_string())
        .text("is_background", is_background.to_string())
        .part(
            "compose",
            reqwest::multipart::Part::bytes(resolved_yaml.into_bytes())
                .file_name(compose_name.to_string())
                .mime_str("text/yaml")?,
        );
    if is_partial_build {
        let target_list: Vec<&str> = build_infos.iter().map(|b| b.svc_name.as_str()).collect();
        form = form.text("target_services", target_list.join(","));
    }
    if let Some(nid) = node_id {
        form = form.text("node_id", nid.to_string());
    }
    if stage_only {
        form = form.text("stage_only", "true".to_string());
    }
    if !grant_capabilities.is_empty() {
        form = form.text("grant_capabilities", grant_capabilities.join(","));
    }

    auth::session_post_multipart(client, server, "/deploy/compose", form).await
}

/// Validate compose YAML via orchestrator and return approved capability grants.
async fn validate_compose_for_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    yaml: &str,
    is_background: bool,
    interactive: bool,
    pregranted: &[String],
) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "compose": yaml,
        "project_id": project_id,
        "is_background": is_background,
    });
    let resp = auth::session_post(client, server, "/compose/validate", &body).await?;

    let ok = resp["report"]["ok"].as_bool().unwrap_or(false);
    let findings = resp["report"]["findings"].as_array().cloned().unwrap_or_default();
    let missing: Vec<String> = resp["missing_capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    println!("  {} Compose compatibility", "::".dimmed());

    #[derive(Default)]
    struct Groups {
        unsupported: Vec<(Option<String>, String)>,
        permission: Vec<(Option<String>, String)>,
        translated: Vec<(Option<String>, String)>,
        overridden: Vec<(Option<String>, String)>,
        supported_fields: std::collections::BTreeMap<String, Vec<String>>,
        supported_other: Vec<(Option<String>, String)>,
    }
    let mut g = Groups::default();

    for f in &findings {
        let disposition = f["disposition"].as_str().unwrap_or("");
        let path = f["path"].as_str().unwrap_or("?");
        let message = f["message"].as_str().unwrap_or("");
        let service = f["service"].as_str().map(|s| s.to_string());
        match disposition {
            "unsupported" => g.unsupported.push((service, format!("{path} — {message}"))),
            "permission_required" => g.permission.push((service, message.to_string())),
            "translated" => g.translated.push((service, message.to_string())),
            "overridden" => g.overridden.push((service, message.to_string())),
            "supported" => {
                if let (Some(svc), Some(field)) =
                    (service.as_deref(), message.strip_suffix(" is supported"))
                {
                    g.supported_fields
                        .entry(svc.to_string())
                        .or_default()
                        .push(field.to_string());
                } else {
                    g.supported_other.push((service, message.to_string()));
                }
            }
            _ => {}
        }
    }

    let print_grouped = |title: &colored::ColoredString, items: &[(Option<String>, String)]| {
        if items.is_empty() {
            return;
        }
        println!("  {}", title);
        // Preserve order: project-level first, then services alphabetically
        let mut project = Vec::new();
        let mut by_svc: std::collections::BTreeMap<String, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (svc, line) in items {
            match svc {
                Some(s) => by_svc.entry(s.clone()).or_default().push(line.as_str()),
                None => project.push(line.as_str()),
            }
        }
        for line in project {
            println!("    {}", line.dimmed());
        }
        for (svc, lines) in by_svc {
            println!("    {}", svc.bold());
            for line in lines {
                println!("      {}", line.dimmed());
            }
        }
    };

    print_grouped(&"Unsupported".red().bold(), &g.unsupported);
    print_grouped(&"Capabilities requested".yellow().bold(), &g.permission);

    let mut supported_items: Vec<(Option<String>, String)> = g.supported_other;
    for (svc, fields) in g.supported_fields {
        supported_items.push((Some(svc), fields.join(", ")));
    }
    print_grouped(&"Supported".green().bold(), &supported_items);
    print_grouped(&"Adapted by LiteBin".cyan().bold(), &g.translated);
    print_grouped(&"Overridden by LiteBin".bold(), &g.overridden);

    if !ok {
        anyhow::bail!("compose file has unsupported fields — fix them and retry");
    }

    let mut grants: Vec<String> = pregranted.to_vec();
    let still_missing: Vec<String> = missing
        .into_iter()
        .filter(|c| !grants.iter().any(|g| g == c))
        .collect();

    if still_missing.is_empty() {
        if grants.is_empty() {
            println!("  {} All good — no extra capabilities required", "✓".green());
        }
        return Ok(grants);
    }

    if !interactive {
        anyhow::bail!(
            "missing required capabilities: {}. Pass --grant-capability for each (e.g. --grant-capability docker-access)",
            still_missing.join(", ")
        );
    }

    println!(
        "  {} This compose file requires: {}",
        "!".yellow(),
        still_missing.join(", ")
    );
    let approve = Confirm::new()
        .with_prompt("Grant these capabilities for this project?")
        .default(false)
        .interact()?;
    if !approve {
        anyhow::bail!("capabilities not granted — aborting deploy");
    }
    grants.extend(still_missing);
    Ok(grants)
}

fn load_compose(project_dir: &Path, compose_name: &str) -> Result<serde_yaml::Value> {
    println!(
        "  {} Found {} — deploying as multi-service",
        "🐳".dimmed(),
        compose_name.cyan()
    );

    let compose_yaml = std::fs::read_to_string(project_dir.join(compose_name))
        .with_context(|| format!("failed to read {}", compose_name))?;
    serde_yaml::from_str(&compose_yaml).with_context(|| "failed to parse compose YAML")
}

fn print_compose_build_summary(compose: &serde_yaml::Value, build_infos: &[BuildInfo]) {
    if build_infos.is_empty() {
        return;
    }

    let total_services = compose
        .get("services")
        .and_then(|s| s.as_mapping())
        .map(|m| m.len())
        .unwrap_or(0);
    let pull_count = total_services.saturating_sub(build_infos.len());
    let pull_info = if pull_count > 0 {
        format!(" ({} pre-built will be pulled by orchestrator)", pull_count)
    } else {
        String::new()
    };
    println!(
        "  {} Found {} services — building {}{}",
        "🐳".dimmed(),
        total_services,
        build_infos.len(),
        pull_info
    );
}

fn print_building_services(build_infos: &[BuildInfo]) {
    if !build_infos.is_empty() {
        println!(
            "  {} Building {} service(s)...",
            "🔨".dimmed(),
            build_infos.len()
        );
    }
}

async fn prepare_compose_deployment(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    compose: &serde_yaml::Value,
    build_infos: &[BuildInfo],
    selected_public: Option<&str>,
    env_mode: EnvSelectMode,
    node_id: Option<&str>,
    platform: Option<&str>,
    ci_mode: bool,
) -> Result<String> {
    let root_env_paths = select_env_files(project_dir, env_mode)?;
    let resolved_images = build_and_upload_services(
        client,
        server,
        project_id,
        project_dir,
        build_infos,
        &root_env_paths,
        node_id,
        platform,
        ci_mode,
    )
    .await?;

    let mut resolved_yaml = rewrite_compose_images(compose, &resolved_images)?;
    if let Some(service_name) = selected_public {
        resolved_yaml = inject_public_label(&resolved_yaml, service_name)?;
    }
    Ok(resolved_yaml)
}

// ── Build & deploy ───────────────────────────────────────────────────────────

async fn build_and_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    port: Option<u16>,
    is_background: bool,
    mut secret: Vec<PathBuf>,
    is_new_project: bool,
    node_id: Option<&str>,
) -> Result<Option<String>> {
    let selected_node = select_target_node(client, server, node_id).await?;

    if let Some(compose_name) = detect_compose_file(project_dir) {
        let nodes = auth::fetch_online_nodes(client, server).await;
        let platform = resolve_platform(&nodes, selected_node.as_deref());
        return deploy_compose(
            client,
            server,
            project_id,
            project_dir,
            compose_name,
            is_new_project,
            is_background,
            selected_node.as_deref(),
            platform.as_deref(),
        )
        .await;
    }

    println!("  🔍 Analyzing project...");
    let info =
        crate::build::detect_project(project_dir).unwrap_or_else(|_| crate::build::ProjectInfo {
            project_type: "Unknown".to_string(),
            package: String::new(),
        });
    let has_dockerfile = project_dir.join("Dockerfile").exists();

    if has_dockerfile {
        println!(
            "  🔍 {} (using Dockerfile)",
            format!("Detected {}", info.project_type).dimmed()
        );
    } else {
        println!(
            "  🔍 {}",
            format!("Detected {}", info.project_type).dimmed()
        );
        if !info.package.is_empty() {
            println!("  📦 Package: {}", info.package.dimmed());
        }
    }

    if secret.is_empty() {
        secret = select_env_files(project_dir, EnvSelectMode::Interactive)?;
    }

    if cfg!(target_os = "windows") && !has_dockerfile {
        let masked = crate::build::gitignored_dirs(project_dir);
        if !masked.is_empty() {
            println!(
                "  🪟  Windows detected — masking [{}]",
                masked.join(", ").dimmed()
            );
        } else {
            println!("  🪟  Windows detected — using Docker for Railpack");
        }
    }

    let image_tag = format!("{}/{}:latest", crate::config::IMAGE_PREFIX, project_id);
    let platform = {
        let nodes = auth::fetch_online_nodes(client, server).await;
        let p = resolve_platform(&nodes, selected_node.as_deref());
        if let Some(ref plat) = p {
            println!("  {} Target platform: {}", "::".dimmed(), plat.cyan());
        }
        p
    };

    let image =
        crate::build::build_project(project_dir, None, &image_tag, secret, false, platform.as_deref())
            .await?;

    println!("  📦 Image built — {}", HumanBytes(image.image_size));
    if image.compressed_size < image.image_size {
        println!("  🗜️  Compressed to {}", HumanBytes(image.compressed_size));
    }

    let image_id = crate::upload::upload_tar(
        client,
        server,
        project_id,
        Path::new(&image.path),
        &image.image_id,
        selected_node.as_deref(),
        false,
    )
    .await?;

    let deploy_spinner = spinner("  🚢 {spinner} {msg}");
    deploy_spinner.set_message(if is_new_project {
        "Staging deployment..."
    } else {
        "Deploying..."
    });
    let deploy_resp = crate::deploy::redeploy(
        client,
        server,
        project_id,
        &image_id,
        port,
        is_background,
        selected_node.as_deref(),
        None,
        None,
        None,
        true,
        is_new_project,
    )
    .await?;

    let url = finish_deploy_response(
        client,
        server,
        project_id,
        selected_node.as_deref(),
        deploy_resp.status,
        deploy_resp.node_id.as_deref().or(selected_node.as_deref()),
        deploy_resp.url.as_deref(),
        is_background,
        &deploy_spinner,
        "Deployment staged",
        "Deploy successful!",
        &format!("Deploy failed for project '{}'", project_id),
        Some(image.path.as_str()),
    )
    .await?;

    Ok(url)
}

async fn finish_deploy_response(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    env_node: Option<&str>,
    status: ProjectStatus,
    stage_node: Option<&str>,
    api_url: Option<&str>,
    is_background: bool,
    deploy_spinner: &ProgressBar,
    staged_label: &str,
    success_label: &str,
    fail_label: &str,
    cleanup_tar: Option<&str>,
) -> Result<Option<String>> {
    if status == ProjectStatus::Unconfigured {
        deploy_spinner.finish_and_clear();
        println!("  {} {}", "✔".green(), staged_label);
        let started = await_runtime_config_and_start(client, server, project_id, stage_node).await?;
        if let Some(path) = cleanup_tar {
            let _ = std::fs::remove_file(path);
        }
        stop_buildkit();
        if !started {
            return Ok(None);
        }
        if is_background {
            return Ok(None);
        }
        return Ok(Some(
            resolve_live_url(client, server, project_id, api_url).await,
        ));
    }

    if status == ProjectStatus::Deploying {
        deploy_spinner.set_message("Waiting for deployment...");
        deploy_spinner.finish_and_clear();
        poll_deploy_status(client, server, project_id, success_label, fail_label).await?;
        show_env_path(server, project_id, env_node);
    } else {
        deploy_spinner.finish_and_clear();
        println!("  {} {}", "✔".green(), success_label);
        show_env_path(server, project_id, env_node);
    }
    println!();

    if let Some(path) = cleanup_tar {
        let _ = std::fs::remove_file(path);
    }
    stop_buildkit();

    if is_background {
        return Ok(None);
    }
    Ok(Some(
        resolve_live_url(client, server, project_id, api_url).await,
    ))
}

async fn deploy_compose(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    compose_name: &str,
    is_new_project: bool,
    is_background: bool,
    node_id: Option<&str>,
    platform: Option<&str>,
) -> Result<Option<String>> {
    let compose = load_compose(project_dir, compose_name)?;
    let selected_public = if is_background {
        None
    } else {
        pick_public_service(&compose)?
    };
    let mut build_infos = collect_build_infos(&compose);
    print_compose_build_summary(&compose, &build_infos);

    let mut is_partial_build = false;
    if !build_infos.is_empty() {
        if !is_new_project && build_infos.len() >= 2 {
            let svc_names: Vec<&str> = build_infos.iter().map(|b| b.svc_name.as_str()).collect();
            loop {
                let choices = vec!["Build all", "Pick specific..."];
                let selection = Select::new()
                    .with_prompt("  🔨 Which services to build?")
                    .items(&choices)
                    .default(0)
                    .interact()?;

                match selection {
                    0 => break,
                    1 => {
                        let chosen = MultiSelect::new()
                            .with_prompt(
                                "  🔨 Select services to build [Space to select, Enter to confirm]",
                            )
                            .items(&svc_names)
                            .interact()?;
                        if chosen.is_empty() {
                            println!(
                                "  {} {}",
                                "!".red(),
                                "No services selected. Pick at least one, or choose 'Build all'."
                                    .yellow()
                            );
                            continue;
                        }
                        let selected_names: Vec<&str> =
                            chosen.iter().map(|&i| svc_names[i]).collect();
                        println!(
                            "  {} Building: {}",
                            "::".dimmed(),
                            selected_names.join(", ").dimmed()
                        );
                        build_infos = chosen.into_iter().map(|i| build_infos[i].clone()).collect();
                        is_partial_build = true;
                        break;
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    print_building_services(&build_infos);

    let resolved_yaml = prepare_compose_deployment(
        client,
        server,
        project_id,
        project_dir,
        &compose,
        &build_infos,
        selected_public.as_deref(),
        EnvSelectMode::InteractiveNoCustomOrder,
        node_id,
        platform,
        false,
    )
    .await?;

    let deploy_spinner = spinner("  🚢 {spinner} {msg}");
    deploy_spinner.set_message(if is_new_project {
        "Staging compose deployment..."
    } else {
        "Deploying compose..."
    });

    let grants = validate_compose_for_deploy(
        client,
        server,
        project_id,
        &resolved_yaml,
        is_background,
        true,
        &[],
    )
    .await?;

    let resp = submit_compose(
        client,
        server,
        project_id,
        compose_name,
        resolved_yaml,
        is_partial_build,
        &build_infos,
        node_id,
        is_new_project,
        is_background,
        &grants,
    )
    .await?;

    let resp_status: ProjectStatus = resp["status"]
        .as_str()
        .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
        .unwrap_or(ProjectStatus::Stopped);
    let resp_node = resp["node_id"].as_str().or(node_id);

    finish_deploy_response(
        client,
        server,
        project_id,
        resp_node,
        resp_status,
        resp_node,
        resp["url"].as_str(),
        is_background,
        &deploy_spinner,
        "Compose deployment staged",
        "Compose deploy successful!",
        &format!("Compose deploy failed for project '{}'", project_id),
        None,
    )
    .await
}

/// Non-interactive compose deploy for CI/`deploy` command usage.
pub async fn deploy_compose_noninteractive(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    compose_name: &str,
    _is_new_project: bool,
    opts: ComposeDeployOpts,
    platform: Option<&str>,
) -> Result<String> {
    let compose = load_compose(project_dir, compose_name)?;
    let selected_public = if opts.is_background {
        None
    } else {
        auto_pick_public_service(&compose)
    };
    let mut build_infos = collect_build_infos(&compose);

    let is_partial_build = if let Some(ref targets) = opts.target_services {
        if !targets.is_empty() {
            build_infos.retain(|b| targets.iter().any(|t| t == &b.svc_name));
            true
        } else {
            false
        }
    } else {
        false
    };

    print_compose_build_summary(&compose, &build_infos);
    print_building_services(&build_infos);

    let resolved_yaml = prepare_compose_deployment(
        client,
        server,
        project_id,
        project_dir,
        &compose,
        &build_infos,
        selected_public.as_deref(),
        EnvSelectMode::AutoAllExceptExample,
        opts.node_id.as_deref(),
        platform,
        true,
    )
    .await?;

    println!("  {} Deploying compose...", "🚢".dimmed());
    let grants = validate_compose_for_deploy(
        client,
        server,
        project_id,
        &resolved_yaml,
        opts.is_background,
        false,
        &opts.grant_capabilities,
    )
    .await?;
    let _resp = submit_compose(
        client,
        server,
        project_id,
        compose_name,
        resolved_yaml,
        is_partial_build,
        &build_infos,
        opts.node_id.as_deref(),
        false,
        opts.is_background,
        &grants,
    )
    .await?;

    println!("  {} Compose deploy submitted.", "🚢".dimmed());
    stop_buildkit();
    Ok(project_id.to_string())
}
