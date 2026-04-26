use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::{Confirm, Input, Select};
use indicatif::HumanBytes;
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
    status: String,
    node_id: Option<String>,
    public_stats: Option<PublicStats>,
}

pub async fn run(
    client: &reqwest::Client,
    server: &str,
    path_override: Option<&str>,
    port_override: Option<u16>,
    secret_override: Vec<std::path::PathBuf>,
) -> Result<()> {
    let project_dir = Path::new(path_override.unwrap_or("."));

    // 1. Fetch existing projects
    let projects_json = auth::session_get(client, server, "/projects").await?;
    let projects: Vec<ProjectInfo> = serde_json::from_value(projects_json).unwrap_or_default();

    // 2. New or existing?
    let choices = vec!["New project", "Existing project"];
    let selection = Select::new()
        .with_prompt("Deploy to")
        .items(&choices)
        .default(0)
        .interact()?;

    if selection == 0 {
        new_project_flow(client, server, project_dir, &projects, port_override, secret_override).await
    } else {
        existing_project_flow(client, server, project_dir, &projects, port_override, secret_override).await
    }
}

// New project

async fn new_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    _projects: &[ProjectInfo],
    port_override: Option<u16>,
    secret_override: Vec<std::path::PathBuf>,
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

    println!("  {} Creating project {}...", "::", name.cyan());
    auth::session_post(client, server, "/projects", &json!({"id": &name}))
        .await
        .with_context(|| format!("failed to create project '{}'", name))?;
    println!("  {} Project created", "✔".green());

    // Generate deploy token
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

    // Port — auto-detect 80 from Dockerfile/compose, otherwise prompt
    let port: u16 = if let Some(p) = port_override {
        p
    } else if detect_port_80(project_dir) {
        println!("  {} Detected exposed port 80", "::".dimmed());
        80
    } else {
        let input: String = Input::new()
            .with_prompt("App port")
            .default("3000".to_string())
            .interact_text()?;
        input
            .parse::<u16>()
            .context("Port must be a number (1-65535)")?
    };

    // Build & deploy
    let url = build_and_deploy(client, server, &name, project_dir, port, secret_override, true, None).await?;

    // Success
    println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
    println!();
    println!("  {} Use this token to redeploy from CI/CD:", "💡".dimmed());
    println!(
        "  {}",
        format!(
            "L8B_TOKEN={} l8b deploy --project {} --port {}",
            token, name, port
        )
        .dimmed()
    );
    println!();

    Ok(())
}

// Existing project

async fn existing_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    projects: &[ProjectInfo],
    port_override: Option<u16>,
    secret_override: Vec<std::path::PathBuf>,
) -> Result<()> {
    if projects.is_empty() {
        anyhow::bail!("No existing projects found. Create one with the 'New project' option.");
    }

    let items: Vec<String> = projects
        .iter()
        .map(|p| {
            let status = match p.status.as_str() {
                "running" => p.status.green().to_string(),
                "stopped" | "unconfigured" => p.status.yellow().to_string(),
                s if s.starts_with("error") => p.status.red().to_string(),
                _ => p.status.clone(),
            };
            let image = p
                .public_stats
                .as_ref()
                .and_then(|ps| ps.image.as_deref())
                .map(short_image)
                .unwrap_or_else(|| "—".to_string());
            let port = p
                .public_stats
                .as_ref()
                .and_then(|ps| ps.mapped_port)
                .map(|p| format!("port {}", p))
                .unwrap_or("—".to_string());
            format!("  {:<20} {:<12} {:<25} {}", p.id, status, image, port)
        })
        .collect();

    let idx = Select::new()
        .with_prompt("Select project")
        .items(&items)
        .interact()?;

    let project = &projects[idx];
    let project_id = &project.id;

    // Action
    let actions = vec!["Redeploy", "Recreate", "Start", "Stop", "Delete"];
    let action_idx = Select::new()
        .with_prompt("Action")
        .items(&actions)
        .default(0)
        .interact()?;

    match action_idx {
        0 => {
            // Redeploy — build, upload, deploy
            // Use existing node_id if set, otherwise show picker
            let existing_node = project.node_id.as_deref();
            // Check for compose file — if found, ports come from compose, no prompt needed
            let compose_paths = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
            let compose_file = compose_paths
                .iter()
                .find(|p| project_dir.join(p).exists());

            let url = if compose_file.is_some() {
                // Multi-service: compose handles ports
                build_and_deploy(client, server, project_id, project_dir, 0, secret_override, false, existing_node).await?
            } else if detect_port_80(project_dir) {
                // Single-service with EXPOSE 80 in Dockerfile
                println!("  {} Detected exposed port 80", "::".dimmed());
                build_and_deploy(client, server, project_id, project_dir, 80, secret_override, false, existing_node).await?
            } else {
                // Single-service: ask for port
                let port: u16 = if let Some(p) = port_override {
                    p
                } else {
                    let input: String = Input::new()
                        .with_prompt("App port")
                        .default("3000".to_string())
                        .interact_text()?;
                    input
                        .parse::<u16>()
                        .context("Port must be a number (1-65535)")?
                };
                build_and_deploy(client, server, project_id, project_dir, port, secret_override, false, existing_node).await?
            };
            println!();
            println!();
            println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
            println!();
        }
        1 => {
            // Recreate — no new build, just recreate container
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
        2 => {
            // Start
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
        3 => {
            // Stop
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
        4 => {
            // Delete — needs confirmation
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

/// Detect if port 80 is exposed in Dockerfile or compose file.
fn detect_port_80(project_dir: &Path) -> bool {
    // Check compose first
    let compose_paths = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
    if let Some(name) = compose_paths.iter().find(|p| project_dir.join(p).exists()) {
        if let Ok(content) = std::fs::read_to_string(project_dir.join(name)) {
            for line in content.lines() {
                let trimmed = line.trim();
                if (trimmed == "- '80'" || trimmed == "- \"80\"") && !trimmed.starts_with('#') {
                    return true;
                }
            }
        }
        return false;
    }

    // Check Dockerfile
    let dockerfile = project_dir.join("Dockerfile");
    if let Ok(content) = std::fs::read_to_string(&dockerfile) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.to_uppercase().starts_with("EXPOSE") {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 && parts[1] == "80" {
                    return true;
                }
            }
        }
    }

    false
}

fn show_env_path(server: &str, project_id: &str) {
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
    println!("  {} Runtime secrets: {}  or  {}",
        "🔒".dimmed(), home_env.yellow(), rel_env.yellow());
    println!("     {}", "(default install path; if custom -InstallDir was used, prepend that path instead)".dimmed());
}

// Build & deploy

async fn build_and_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    port: u16,
    mut secret: Vec<std::path::PathBuf>,
    is_new_project: bool,
    node_id: Option<&str>,
) -> Result<String> {
    // Node selection: if project already has a node_id, reuse it (sticky).
    // Otherwise, show picker if multiple nodes are online.
    let selected_node = if node_id.is_some() {
        node_id.map(|s| s.to_string())
    } else {
        let nodes = auth::fetch_online_nodes(client, server).await;
        match nodes.len() {
            0 => None,
            1 => Some(nodes[0].id.clone()),
            _ => {
                let mut items: Vec<String> = nodes
                    .iter()
                    .map(|n| format!("  {} ({})", n.name, n.id))
                    .collect();
                items.insert(0, "  Auto (least loaded)".to_string());
                let idx = Select::new()
                    .with_prompt("Select target node")
                    .items(&items)
                    .default(0)
                    .interact()?;
                if idx == 0 {
                    None // Auto — let server decide
                } else {
                    Some(nodes[idx - 1].id.clone())
                }
            }
        }
    };

    // Check for docker-compose.yaml → use compose deploy endpoint
    let compose_paths = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
    let compose_file = compose_paths
        .iter()
        .find(|p| project_dir.join(p).exists());

    if let Some(compose_name) = compose_file {
        return deploy_compose(client, server, project_id, project_dir, compose_name, is_new_project, selected_node.as_deref()).await;
    }

    // Detect project type
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

    // 2. Secret selection (interactive only)
    if secret.is_empty() {
        let mut env_files: Vec<_> = std::fs::read_dir(project_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".env"))
            .collect();

        // Sort by precedence: templates first (.env.example), base (.env), overrides last (.env.local, .env.production)
        // Later files override earlier ones, so highest priority file comes last
        env_files.sort_by(|a, b| {
            let score = |name: &str| {
                if name == ".env.example" { 0 }       // template — lowest priority
                else if name == ".env" { 1 }           // base defaults
                else if name.contains(".local") { 3 }  // local overrides
                else if name.contains(".prod") { 3 }   // production overrides
                else { 2 }                             // generic .env.* (e.g. .env.development)
            };
            score(a).cmp(&score(b)).then(a.cmp(b))
        });

        if !env_files.is_empty() {
            loop {
                let choices = vec!["Yes (all / standard order)", "No", "Pick specific...", "Custom order (manual input)"];
                let selection = Select::new()
                    .with_prompt("  🔒 Found .env files. Include build-time secrets?")
                    .items(&choices)
                    .default(0)
                    .interact()?;

                match selection {
                    0 => {
                        // Yes (all / standard order)
                        println!("  {} Using standard merge order (later files override earlier ones):", "::".dimmed());
                        println!("     {}", env_files.join(" < ").dimmed());
                        for name in &env_files {
                            secret.push(project_dir.join(name));
                        }
                        break;
                    }
                    1 => {
                        // No
                        println!("  {} No build-time secrets included", "::".dimmed());
                        break;
                    }
                    2 => {
                        // Pick specific...
                        let defaults = vec![false; env_files.len()];

                        let chosen = dialoguer::MultiSelect::new()
                            .with_prompt("  🔒 Select secrets (Standard merge order applies) [Space to select, Enter to confirm]")
                            .items(&env_files)
                            .defaults(&defaults)
                            .interact()?;

                        if !chosen.is_empty() {
                            let mut selected_names = Vec::new();
                            for idx in chosen {
                                let name = &env_files[idx];
                                selected_names.push(name.as_str());
                                secret.push(project_dir.join(name));
                            }
                            println!("  {} Merging: {}", "::".dimmed(), selected_names.join(" < ").dimmed());
                            break;
                        } else {
                            println!("  {} {}", "!".red(), "No files selected. Pick at least one, or choose 'No' to continue without secrets.".yellow());
                            continue; // Re-prompt
                        }
                    }
                    3 => {
                        // Custom order (manual input)
                        println!("  {} Available files: {}", "::".dimmed(), env_files.join(", ").dimmed());
                        let input: String = Input::new()
                            .with_prompt("  🔒 Enter filenames in merge order (space separated, e.g. .env .env.local)")
                            .interact_text()?;
                        
                        let parts: Vec<&str> = input.split_whitespace().collect();
                        for part in &parts {
                            let path = project_dir.join(part);
                            if path.exists() {
                                secret.push(path);
                            } else {
                                println!("  {} {} does not exist, skipping", "!".yellow(), part);
                            }
                        }
                        if !secret.is_empty() {
                            println!("  {} Merging in your exact order: {}", "::".dimmed(), parts.join(" < ").dimmed());
                            break;
                        } else {
                            println!("  {} {}", "!".red(), "No valid files entered.".yellow());
                            continue; // Re-prompt
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    // Build
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
    let image = crate::build::build_project(project_dir, None, &image_tag, secret, false).await?;

    println!("  📦 Image built — {}", HumanBytes(image.image_size));

    // Compress
    if image.compressed_size < image.image_size {
        println!("  🗜️  Compressed to {}", HumanBytes(image.compressed_size));
    }

    // Upload
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

    // Deploy
    let deploy_spinner = indicatif::ProgressBar::new_spinner();
    deploy_spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .template("  🚢 {spinner} {msg}")
            .unwrap(),
    );
    deploy_spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    deploy_spinner.set_message("Deploying...");
    let deploy_resp = crate::deploy::redeploy(
        client, server, project_id, &image_id, port, selected_node.as_deref(), None, None, None, true,
    )
    .await?;
    deploy_spinner.finish_and_clear();
    println!("  {} Deploy successful!", "✔".green());
    show_env_path(server, project_id);
    println!();

    // Clean up temp tar file
    let _ = std::fs::remove_file(&image.path);

    // Stop BuildKit container (started with --rm, so stop = auto-remove)
    println!("  🧹 Stopping BuildKit...");
    let _ = std::process::Command::new("docker")
        .args(["stop", "buildkit"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    Ok(deploy_resp.url)
}

fn short_image(image: &str) -> String {
    let hash = image.strip_prefix("sha256:").unwrap_or(image);
    if hash.len() > 12 {
        hash[..12].to_string()
    } else {
        hash.to_string()
    }
}

/// Deploy a multi-service project via docker-compose.yaml.
/// For services with `build:`, builds each image with Railpack, uploads to orchestrator.
/// For services with `image:`, the orchestrator pulls from registry.
/// Sends resolved compose (all `build:` → `image: sha256:...`) to the orchestrator.
async fn deploy_compose(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    compose_name: &str,
    is_new_project: bool,
    node_id: Option<&str>,
) -> Result<String> {
    let compose_path = project_dir.join(compose_name);
    println!("  {} Found {} — deploying as multi-service", "🐳".dimmed(), compose_name.cyan());

    let compose_yaml = std::fs::read_to_string(&compose_path)
        .with_context(|| format!("failed to read {}", compose_name))?;

    // Parse compose YAML to find services with `build:`
    let compose: serde_yaml::Value = serde_yaml::from_str(&compose_yaml)
        .with_context(|| "failed to parse compose YAML")?;

    // Phase 1: Build and upload images for services with `build:`
    // Collect build info into owned values so we can drop the borrow on `compose`
    #[derive(Clone)]
    struct BuildInfo {
        svc_name: String,
        build_context: String,
        dockerfile: Option<String>,
    }
    let mut build_infos = Vec::new();
    let total_services = compose.get("services").and_then(|s| s.as_mapping()).map(|m| m.len()).unwrap_or(0);
    if let Some(services) = compose.get("services").and_then(|s| s.as_mapping()) {
        for (svc_name, svc_config) in services {
            if svc_config.get("build").is_some() && svc_config.get("image").is_none() {
                let name = svc_name.as_str().unwrap_or_default().to_string();

                // Support both string and object forms of build:
                //   String: build: ./api
                //   Object: build: { context: ./api, dockerfile: Dockerfile.dev }
                let (ctx, dockerfile) = match svc_config.get("build") {
                    Some(b) if b.as_str().is_some() => {
                        (b.as_str().unwrap_or(&name).to_string(), None)
                    }
                    Some(b) if b.as_mapping().is_some() => {
                        let build_map = b.as_mapping().unwrap();
                        let context = build_map.get("context")
                            .and_then(|c| c.as_str())
                            .unwrap_or(&name)
                            .to_string();
                        let df = build_map.get("dockerfile")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());
                        (context, df)
                    }
                    _ => (name.clone(), None),
                };

                build_infos.push(BuildInfo { svc_name: name, build_context: ctx, dockerfile });
            }
        }
    }

    // Track which services to actually build (may be filtered by user on redeploy)
    let mut is_partial_build = false;

    if !build_infos.is_empty() {
        let pull_count = total_services - build_infos.len();
        let pull_info = if pull_count > 0 {
            format!(" ({} pre-built will be pulled by orchestrator)", pull_count)
        } else {
            String::new()
        };
        println!("  {} Found {} services — building {}{}", "🐳".dimmed(), total_services, build_infos.len(), pull_info);

        // On redeploy with 2+ buildable services, ask which to build
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
                    0 => {
                        // Build all
                        break;
                    }
                    1 => {
                        // Pick specific
                        let defaults = vec![false; svc_names.len()];
                        let chosen = dialoguer::MultiSelect::new()
                            .with_prompt("  🔨 Select services to build [Space to select, Enter to confirm]")
                            .items(&svc_names)
                            .defaults(&defaults)
                            .interact()?;

                        if !chosen.is_empty() {
                            let selected: Vec<usize> = chosen;
                            let selected_names: Vec<&str> = selected.iter().map(|&i| svc_names[i]).collect();
                            println!("  {} Building: {}", "::".dimmed(), selected_names.join(", ").dimmed());
                            let mut filtered = Vec::new();
                            for idx in selected {
                                filtered.push(build_infos[idx].clone());
                            }
                            build_infos = filtered;
                            is_partial_build = true;
                            break;
                        } else {
                            println!("  {} {}", "!".red(), "No services selected. Pick at least one, or choose 'Build all'.".yellow());
                            continue;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }

        println!("  {} Building {} service(s)...", "🔨".dimmed(), build_infos.len());
    }

    let mut resolved_images: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Collect .env files from the project root (where docker-compose.yml lives)
    let mut root_env_files: Vec<String> = std::fs::read_dir(project_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".env"))
        .collect();

    root_env_files.sort_by(|a, b| {
        let score = |name: &str| {
            if name == ".env.example" { 0 }
            else if name == ".env" { 1 }
            else if name.contains(".local") { 3 }
            else if name.contains(".prod") { 3 }
            else { 2 }
        };
        score(a).cmp(&score(b)).then(a.cmp(b))
    });

    let mut root_env_paths: Vec<std::path::PathBuf> = Vec::new();
    if !root_env_files.is_empty() {
        loop {
            let choices = vec!["Yes (all / standard order)", "No", "Pick specific..."];
            let selection = Select::new()
                .with_prompt("  🔒 Found .env files. Include build-time secrets?")
                .items(&choices)
                .default(0)
                .interact()?;

            match selection {
                0 => {
                    println!("  {} Using standard merge order (later files override earlier ones):", "::".dimmed());
                    println!("     {}", root_env_files.join(" < ").dimmed());
                    for name in &root_env_files {
                        root_env_paths.push(project_dir.join(name));
                    }
                    break;
                }
                1 => {
                    println!("  {} No build-time secrets included", "::".dimmed());
                    break;
                }
                2 => {
                    let chosen = dialoguer::MultiSelect::new()
                        .with_prompt("  🔒 Select secrets [Space to select, Enter to confirm]")
                        .items(&root_env_files)
                        .interact()?;

                    if !chosen.is_empty() {
                        for idx in chosen {
                            root_env_paths.push(project_dir.join(&root_env_files[idx]));
                        }
                        break;
                    } else {
                        println!("  {} {}", "!".red(), "No files selected. Pick at least one, or choose 'No' to continue without secrets.".yellow());
                        continue;
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    for info in &build_infos {
        let svc_dir = project_dir.join(&info.build_context).canonicalize()
            .with_context(|| format!("build context '{}' does not exist for service '{}'", info.build_context, info.svc_name))?;
        // Strip Windows extended-length path prefix (\\?\) — Docker doesn't understand it
        let svc_dir = Path::new(svc_dir.to_str().unwrap().trim_start_matches(r"\\?\"));

        // Check for .env files in the service directory + project root
        let mut secret = root_env_paths.clone();
        if let Ok(entries) = std::fs::read_dir(&svc_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(".env") && !secret.iter().any(|p| p.file_name() == Some(entry.file_name().as_os_str())) {
                    secret.push(entry.path());
                }
            }
        }

        let image_tag = format!("{}/{}-{}", crate::config::IMAGE_PREFIX, project_id, info.svc_name);
        println!("    {} {} ({})", "→".dimmed(), info.svc_name.cyan(), svc_dir.display());

        // Build with Railpack (uses Dockerfile if present, otherwise auto-detects)
        let saved_image = crate::build::build_project(&svc_dir, info.dockerfile.as_deref(), &image_tag, secret, false).await?;
        println!("    {} {} — {}", "  ✓".green(), info.svc_name, HumanBytes(saved_image.compressed_size));

        // Upload tar to orchestrator
        let image_id = crate::upload::upload_tar(
            client, server, project_id,
            Path::new(&saved_image.path), &saved_image.image_id,
            node_id, false,
        ).await?;

        resolved_images.insert(info.svc_name.clone(), image_id);

        // Clean up local tar
        let _ = std::fs::remove_file(&saved_image.path);
    }

    // Phase 2: Build resolved compose YAML
    // Replace `build:` with `image: sha256:...` for built services
    let mut resolved_compose = compose.clone();
    if let Some(services_map) = resolved_compose.get_mut("services").and_then(|s| s.as_mapping_mut()) {
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

    let resolved_yaml = serde_yaml::to_string(&resolved_compose)?;

    // Phase 3: Send resolved compose to orchestrator
    let deploy_spinner = indicatif::ProgressBar::new_spinner();
    deploy_spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .template("  🚢 {spinner} {msg}")
            .unwrap(),
    );
    deploy_spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    deploy_spinner.set_message("Deploying compose...");

    let mut form = reqwest::multipart::Form::new()
        .text("project_id", project_id.to_string())
        .part("compose", reqwest::multipart::Part::bytes(resolved_yaml.into_bytes())
            .file_name(compose_name.to_string())
            .mime_str("text/yaml")?);
    if is_partial_build {
        let target_list: Vec<&str> = build_infos.iter().map(|b| b.svc_name.as_str()).collect();
        form = form.text("target_services", target_list.join(","));
    }
    if let Some(nid) = node_id {
        form = form.text("node_id", nid.to_string());
    }

    let resp = auth::session_post_multipart(client, server, "/deploy/compose", form).await?;
    deploy_spinner.finish_and_clear();

    println!("  {} Compose deploy successful!", "✔".green());
    show_env_path(server, project_id);
    println!();

    // Stop BuildKit container
    println!("  🧹 Stopping BuildKit...");
    let _ = std::process::Command::new("docker")
        .args(["stop", "buildkit"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let fallback_url = format!("{}.{}", project_id, server);
    let url = resp["url"].as_str().unwrap_or(&fallback_url);
    Ok(url.to_string())
}

/// Options for non-interactive compose deploy (used by `deploy` command).
pub struct ComposeDeployOpts {
    /// If Some, only build these services (no interactive prompt).
    pub target_services: Option<Vec<String>>,
    /// Target node ID (optional).
    pub node_id: Option<String>,
}

/// Non-interactive compose deploy for CI/`deploy` command usage.
/// Builds all (or selected) services, uploads images, and sends resolved compose to orchestrator.
pub async fn deploy_compose_noninteractive(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    compose_name: &str,
    _is_new_project: bool,
    opts: ComposeDeployOpts,
) -> Result<String> {
    let compose_path = project_dir.join(compose_name);
    println!("  {} Found {} — deploying as multi-service", "🐳".dimmed(), compose_name.cyan());

    let compose_yaml = std::fs::read_to_string(&compose_path)
        .with_context(|| format!("failed to read {}", compose_name))?;

    let compose: serde_yaml::Value = serde_yaml::from_str(&compose_yaml)
        .with_context(|| "failed to parse compose YAML")?;

    // Collect build info
    #[derive(Clone)]
    struct BuildInfo {
        svc_name: String,
        build_context: String,
        dockerfile: Option<String>,
    }
    let mut build_infos = Vec::new();
    if let Some(services) = compose.get("services").and_then(|s| s.as_mapping()) {
        for (svc_name, svc_config) in services {
            if svc_config.get("build").is_some() && svc_config.get("image").is_none() {
                let name = svc_name.as_str().unwrap_or_default().to_string();
                let (ctx, dockerfile) = match svc_config.get("build") {
                    Some(b) if b.as_str().is_some() => {
                        (b.as_str().unwrap_or(&name).to_string(), None)
                    }
                    Some(b) if b.as_mapping().is_some() => {
                        let build_map = b.as_mapping().unwrap();
                        let context = build_map.get("context")
                            .and_then(|c| c.as_str())
                            .unwrap_or(&name)
                            .to_string();
                        let df = build_map.get("dockerfile")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());
                        (context, df)
                    }
                    _ => (name.clone(), None),
                };
                build_infos.push(BuildInfo { svc_name: name, build_context: ctx, dockerfile });
            }
        }
    }

    // Filter to target services if specified
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

    let total_services = compose.get("services").and_then(|s| s.as_mapping()).map(|m| m.len()).unwrap_or(0);
    if !build_infos.is_empty() {
        let pull_count = total_services - build_infos.len();
        let pull_info = if pull_count > 0 {
            format!(" ({} pre-built will be pulled by orchestrator)", pull_count)
        } else {
            String::new()
        };
        println!("  {} Found {} services — building {}{}", "🐳".dimmed(), total_services, build_infos.len(), pull_info);
        println!("  {} Building {} service(s)...", "🔨".dimmed(), build_infos.len());
    }

    let mut resolved_images: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Auto-select all .env files (non-interactive)
    let mut root_env_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut root_env_files: Vec<String> = std::fs::read_dir(project_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".env") && name != ".env.example")
        .collect();
    root_env_files.sort();
    for name in &root_env_files {
        root_env_paths.push(project_dir.join(name));
    }
    if !root_env_paths.is_empty() {
        println!("  {} Using .env files: {}", "::".dimmed(), root_env_files.join(", ").dimmed());
    }

    for info in &build_infos {
        let svc_dir = project_dir.join(&info.build_context).canonicalize()
            .with_context(|| format!("build context '{}' does not exist for service '{}'", info.build_context, info.svc_name))?;
        let svc_dir = Path::new(svc_dir.to_str().unwrap().trim_start_matches(r"\\?\"));

        let mut secret = root_env_paths.clone();
        if let Ok(entries) = std::fs::read_dir(&svc_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(".env") && !secret.iter().any(|p| p.file_name() == Some(entry.file_name().as_os_str())) {
                    secret.push(entry.path());
                }
            }
        }

        let image_tag = format!("{}/{}-{}", crate::config::IMAGE_PREFIX, project_id, info.svc_name);
        println!("    {} {} ({})", "→".dimmed(), info.svc_name.cyan(), svc_dir.display());

        let saved_image = crate::build::build_project(&svc_dir, info.dockerfile.as_deref(), &image_tag, secret, true).await?;
        println!("    {} {} — {}", "  ✓".green(), info.svc_name, HumanBytes(saved_image.compressed_size));

        let image_id = crate::upload::upload_tar(
            client, server, project_id,
            Path::new(&saved_image.path), &saved_image.image_id,
            opts.node_id.as_deref(), true,
        ).await?;

        resolved_images.insert(info.svc_name.clone(), image_id);
        let _ = std::fs::remove_file(&saved_image.path);
    }

    // Build resolved compose YAML
    let mut resolved_compose = compose.clone();
    if let Some(services_map) = resolved_compose.get_mut("services").and_then(|s| s.as_mapping_mut()) {
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

    let resolved_yaml = serde_yaml::to_string(&resolved_compose)?;

    println!("  {} Deploying compose...", "🚢".dimmed());

    let mut form = reqwest::multipart::Form::new()
        .text("project_id", project_id.to_string())
        .part("compose", reqwest::multipart::Part::bytes(resolved_yaml.into_bytes())
            .file_name(compose_name.to_string())
            .mime_str("text/yaml")?);
    if is_partial_build {
        let target_list: Vec<&str> = build_infos.iter().map(|b| b.svc_name.as_str()).collect();
        form = form.text("target_services", target_list.join(","));
    }
    if let Some(ref nid) = opts.node_id {
        form = form.text("node_id", nid.clone());
    }

    let resp = auth::session_post_multipart(client, server, "/deploy/compose", form).await?;

    println!("  {} Compose deploy successful!", "✔".green());
    show_env_path(server, project_id);
    println!();

    // Stop BuildKit container
    println!("  🧹 Stopping BuildKit...");
    let _ = std::process::Command::new("docker")
        .args(["stop", "buildkit"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let fallback_url = format!("{}.{}", project_id, server);
    let url = resp["url"].as_str().unwrap_or(&fallback_url);
    Ok(url.to_string())
}

/// Detect compose file in the given directory. Returns the filename or None.
pub fn detect_compose_file(project_dir: &Path) -> Option<&'static str> {
    const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
    COMPOSE_FILES.iter().find(|p| project_dir.join(p).exists()).copied()
}
