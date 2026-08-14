use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use dialoguer::{MultiSelect, Select};
use indicatif::HumanBytes;
use litebin_common::types::ProjectStatus;

use crate::auth;

use super::ComposeDeployOpts;
use super::build_upload::{collect_build_infos, prepare_compose_deployment, submit_compose};
use super::env::EnvSelectMode;
use super::flow::{await_runtime_config_and_start, poll_deploy_status, resolve_upload_mode, select_target_node};
use super::public_service::{auto_pick_public_service, pick_public_service};
use super::ui::{
    detect_compose_file, load_compose, print_building_services, print_compose_build_summary, resolve_live_url,
    resolve_platform, show_env_path, spinner, stop_buildkit,
};
use super::validate::validate_compose_for_deploy;

// ── Build & deploy ───────────────────────────────────────────────────────────

pub(super) async fn build_and_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    port: Option<u16>,
    is_background: bool,
    mut secret: Vec<std::path::PathBuf>,
    is_new_project: bool,
    node_id: Option<&str>,
) -> Result<Option<String>> {
    let selected_node = select_target_node(client, server, node_id).await?;
    let nodes = auth::fetch_online_nodes(client, server).await;
    let upload_mode = resolve_upload_mode(&nodes, selected_node.as_deref(), false, crate::upload::UploadMode::Auto);

    if let Some(compose_name) = detect_compose_file(project_dir) {
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
            upload_mode,
        )
        .await;
    }

    println!("  🔍 Analyzing project...");
    let info = crate::build::detect_project(project_dir)
        .unwrap_or_else(|_| crate::build::ProjectInfo { project_type: "Unknown".to_string(), package: String::new() });
    let has_dockerfile = project_dir.join("Dockerfile").exists();

    if has_dockerfile {
        println!("  🔍 {} (using Dockerfile)", format!("Detected {}", info.project_type).dimmed());
    } else {
        println!("  🔍 {}", format!("Detected {}", info.project_type).dimmed());
        if !info.package.is_empty() {
            println!("  📦 Package: {}", info.package.dimmed());
        }
    }

    if secret.is_empty() {
        secret = super::env::select_env_files(project_dir, EnvSelectMode::Interactive)?;
    }

    if cfg!(target_os = "windows") && !has_dockerfile {
        let masked = crate::build::gitignored_dirs(project_dir);
        if !masked.is_empty() {
            println!("  🪟  Windows detected — masking [{}]", masked.join(", ").dimmed());
        } else {
            println!("  🪟  Windows detected — using Docker for Railpack");
        }
    }

    let image_tag = format!("{}/{}:latest", crate::config::IMAGE_PREFIX, project_id);
    let platform = {
        let p = resolve_platform(&nodes, selected_node.as_deref());
        if let Some(ref plat) = p {
            println!("  {} Target platform: {}", "::".dimmed(), plat.cyan());
        }
        p
    };

    let image = crate::build::build_project(project_dir, None, &image_tag, secret, false, platform.as_deref()).await?;

    println!("  📦 Image built — {}", HumanBytes(image.image_size));
    if image.compressed_size < image.image_size {
        println!("  🗜️  Compressed to {}", HumanBytes(image.compressed_size));
    }

    let image_id = crate::upload::upload_image(
        client,
        server,
        project_id,
        Path::new(&image.path),
        &image.image_id,
        selected_node.as_deref(),
        upload_mode,
        false,
    )
    .await?;

    let deploy_spinner = spinner("  🚢 {spinner} {msg}");
    deploy_spinner.set_message(if is_new_project { "Staging deployment..." } else { "Deploying..." });
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
        &[],
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

pub(super) async fn finish_deploy_response(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    env_node: Option<&str>,
    status: ProjectStatus,
    stage_node: Option<&str>,
    api_url: Option<&str>,
    is_background: bool,
    deploy_spinner: &indicatif::ProgressBar,
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
        return Ok(Some(resolve_live_url(client, server, project_id, api_url).await));
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
    Ok(Some(resolve_live_url(client, server, project_id, api_url).await))
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
    upload_flag: crate::upload::UploadMode,
) -> Result<Option<String>> {
    let compose = load_compose(project_dir, compose_name)?;
    let selected_public = if is_background { None } else { pick_public_service(&compose)? };
    let mut build_infos = collect_build_infos(&compose);
    print_compose_build_summary(&compose, &build_infos);

    let mut is_partial_build = false;
    if !build_infos.is_empty() {
        if !is_new_project && build_infos.len() >= 2 {
            let svc_names: Vec<&str> = build_infos.iter().map(|b| b.svc_name.as_str()).collect();
            loop {
                let choices = vec!["Build all", "Pick specific..."];
                let selection =
                    Select::new().with_prompt("  🔨 Which services to build?").items(&choices).default(0).interact()?;

                match selection {
                    0 => break,
                    1 => {
                        let chosen = MultiSelect::new()
                            .with_prompt("  🔨 Select services to build [Space to select, Enter to confirm]")
                            .items(&svc_names)
                            .interact()?;
                        if chosen.is_empty() {
                            println!(
                                "  {} {}",
                                "!".red(),
                                "No services selected. Pick at least one, or choose 'Build all'.".yellow()
                            );
                            continue;
                        }
                        let selected_names: Vec<&str> = chosen.iter().map(|&i| svc_names[i]).collect();
                        println!("  {} Building: {}", "::".dimmed(), selected_names.join(", ").dimmed());
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

    let upload_mode = resolve_upload_mode(&auth::fetch_online_nodes(client, server).await, node_id, false, upload_flag);

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
        upload_mode,
        false,
    )
    .await?;

    let deploy_spinner = spinner("  🚢 {spinner} {msg}");
    deploy_spinner.set_message(if is_new_project { "Staging compose deployment..." } else { "Deploying compose..." });

    let grants = validate_compose_for_deploy(
        client,
        server,
        project_id,
        &resolved_yaml,
        is_background,
        true,
        &[],
        Some(&deploy_spinner),
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
        // Interactive deploys always stage first so the env-edit checkpoint fires
        // (user updates the node .env, then starts) — for both first deploy and redeploy.
        true,
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
    let selected_public = if opts.is_background { None } else { auto_pick_public_service(&compose) };
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
        opts.upload,
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
        None,
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
