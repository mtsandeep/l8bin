use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::{Confirm, Input, Select};
use indicatif::HumanBytes;
use serde_json::json;

use crate::auth;

#[derive(serde::Deserialize)]
struct ProjectInfo {
    id: String,
    image: Option<String>,
    #[allow(dead_code)]
    internal_port: Option<u16>,
    mapped_port: Option<u16>,
    status: String,
}

pub async fn run(
    client: &reqwest::Client,
    server: &str,
    path_override: Option<&str>,
    port_override: Option<u16>,
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
        new_project_flow(client, server, project_dir, &projects, port_override).await
    } else {
        existing_project_flow(client, server, project_dir, &projects, port_override).await
    }
}

// New project

async fn new_project_flow(
    client: &reqwest::Client,
    server: &str,
    project_dir: &Path,
    _projects: &[ProjectInfo],
    port_override: Option<u16>,
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

    // Port
    let port: u16 = if let Some(p) = port_override {
        p
    } else {
        let input: String = Input::new()
            .with_prompt("App port")
            .default("3000".to_string())
            .interact_text()?;
        input.parse::<u16>().context("Port must be a number (1-65535)")?
    };

    // Build & deploy
    let url = build_and_deploy(client, server, &name, project_dir, port).await?;

    // Success
    println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
    println!();
    println!(
        "  {} Use this token to redeploy from CI/CD:",
        "💡".dimmed()
    );
    println!("  {}", format!("L8B_TOKEN={} l8b deploy --project {} --port {}", token, name, port).dimmed());
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
            let image = p.image.as_deref().map(short_image).unwrap_or_else(|| "—".to_string());
            let port = p.mapped_port
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
            let port: u16 = if let Some(p) = port_override {
                p
            } else {
                let input: String = Input::new()
                    .with_prompt("App port")
                    .default("3000".to_string())
                    .interact_text()?;
                input.parse::<u16>().context("Port must be a number (1-65535)")?
            };

            let url = build_and_deploy(client, server, project_id, project_dir, port).await?;
            println!();
            println!();
            println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
            println!();
        }
        1 => {
            // Recreate — no new build, just recreate container
            println!("  {} Recreating {}...", "::", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/recreate", project_id), &json!({}))
                .await?;
            println!("  {} Recreated", "✔".green());
            println!();
        }
        2 => {
            // Start
            println!("  {} Starting {}...", "::", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/start", project_id), &json!({}))
                .await?;
            println!("  {} Started", "✔".green());
            println!();
        }
        3 => {
            // Stop
            println!("  {} Stopping {}...", "::", project_id.cyan());
            auth::session_post(client, server, &format!("/projects/{}/stop", project_id), &json!({}))
                .await?;
            println!("  {} Stopped", "✔".green());
            println!();
        }
        4 => {
            // Delete — needs confirmation
            let confirmed = Confirm::new()
                .with_prompt(format!("Delete project {}? This cannot be undone.", project_id.red()))
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

// Build & deploy

async fn build_and_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    port: u16,
) -> Result<String> {
    // Detect project type
    let info = crate::build::detect_project(project_dir).unwrap_or_else(|_| crate::build::ProjectInfo {
        project_type: "Unknown".to_string(),
        package: String::new(),
    });
    let has_dockerfile = project_dir.join("Dockerfile").exists();

    if has_dockerfile {
        println!("  🔍 {} (using Dockerfile)", format!("Detected {}", info.project_type).dimmed());
    } else {
        println!("  🔍 {}", format!("Detected {}", info.project_type).dimmed());
        if !info.package.is_empty() {
            println!("  📦 Package: {}", info.package.dimmed());
        }
    }

    // Build
    if cfg!(target_os = "windows") && !has_dockerfile {
        println!("  🪟  Windows detected — using Docker to run Railpack + BuildKit");
    }
    let image_tag = format!("{}/{}:latest", crate::config::IMAGE_PREFIX, project_id);
    let image = crate::build::build_project_quiet(project_dir, None, &image_tag).await?;

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
        None,
    )
    .await?;

    // Deploy
    let deploy_spinner = indicatif::ProgressBar::new_spinner();
    deploy_spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .template("  🚢 {spinner} {msg}")
            .unwrap()
    );
    deploy_spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    deploy_spinner.set_message("Deploying...");
    let deploy_resp = crate::deploy::deploy(
        client,
        server,
        project_id,
        &image_id,
        port,
        None,
        None,
        None,
        None,
        true,
    )
    .await?;
    deploy_spinner.finish_and_clear();
    println!("  {} Deploy successful!", "✔".green());
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
