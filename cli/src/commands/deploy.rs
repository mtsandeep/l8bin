use std::path::PathBuf;

use anyhow::{Result, bail};
use colored::Colorize;
use litebin_common::types::ProjectStatus;

use crate::auth;
use crate::build;
use crate::ci::CiMode;
use crate::config;
use crate::deploy as deploy_cmd;
use crate::ship;
use crate::status;
use crate::upload;

pub(crate) struct DeployArgs {
    pub project: String,
    pub port: u16,
    pub background: bool,
    pub path: PathBuf,
    pub node: Option<String>,
    pub dockerfile: Option<String>,
    pub cmd: Option<String>,
    pub memory: Option<i64>,
    pub cpu: Option<f64>,
    pub no_auto_stop: bool,
    pub secret: Vec<PathBuf>,
    pub compose: bool,
    pub service: Vec<String>,
    pub grant_capability: Vec<String>,
    pub upload: upload::UploadMode,
}

pub(crate) async fn run(
    args: DeployArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &CiMode,
) -> Result<()> {
    let DeployArgs {
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
        upload,
    } = args;

    if !project.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("Project name must only contain lowercase letters, numbers, and hyphens");
    }

    let cfg = config::CliConfig::load(server_flag, token_flag)?;

    let client = auth::authenticated_client(&cfg)?;
    let server = auth::resolve_server(&cfg)?;

    // Resolve effective node: project's sticky node_id takes precedence over --node flag
    let existing_project = auth::session_get(&client, &server, &format!("/projects/{}", project)).await.ok();
    let effective_node = if let Some(proj_json) = existing_project.as_ref() {
        let existing_node = proj_json.get("node_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        if let Some(pinned) = existing_node
            && node.is_some()
            && Some(pinned) != node.as_deref()
        {
            eprintln!("  Note: --node ignored, project is pinned to node '{}'", pinned);
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

        if compose && compose_file.is_none() {
            bail!("--compose flag specified but no compose file found in {}", path.display());
        }

        let target_services = if service.is_empty() { None } else { Some(service) };

        // Resolve target platform from node architecture
        let nodes = auth::fetch_online_nodes(&client, &server).await;
        let platform = ship::resolve_platform(&nodes, effective_node.as_deref());

        ship::deploy_compose_noninteractive(
            &client,
            &server,
            &project,
            &path,
            compose_name,
            true,
            ship::ComposeDeployOpts {
                target_services,
                node_id: effective_node.clone(),
                grant_capabilities: grant_capability,
                is_background: effective_background,
                upload,
            },
            platform.as_deref(),
        )
        .await?;

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

        let image = build::build_project(
            &path,
            dockerfile.as_deref(),
            &image_tag,
            secret,
            ci_mode.enabled,
            platform.as_deref(),
        )
        .await?;

        ci_mode.println("Uploading image...");
        let image_id = upload::upload_image(
            &client,
            &server,
            &project,
            std::path::Path::new(&image.path),
            &image.image_id,
            effective_node.as_deref(),
            upload,
            ci_mode.enabled,
        )
        .await?;

        ci_mode.println("Deploying...");
        let response = deploy_cmd::deploy_or_redeploy(
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
            &grant_capability,
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

    Ok(())
}
