use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use litebin_common::types::ProjectStatus;

use serde::Serialize;

use crate::auth;
use crate::build;
use crate::ci::CiMode;
use crate::config;
use crate::deploy as deploy_cmd;
use crate::out::Out;
use crate::ship;
use crate::status;
use crate::upload;

#[derive(Serialize)]
struct DeployOutcome {
    project_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    background: bool,
    duration_secs: u64,
}

/// Report a finished (or still-in-progress) deploy: JSON payload in --json mode,
/// human lines otherwise. `deploying` after the poll timeout is not an error —
/// agents read the `status` field.
fn finish_deploy(
    out: &Out,
    project: &str,
    final_status: Option<ProjectStatus>,
    url: Option<String>,
    background: bool,
    started: std::time::Instant,
) -> Result<()> {
    let duration = started.elapsed().as_secs();
    let status_label = match final_status {
        Some(ProjectStatus::Error) => {
            return Err(crate::out::fail(
                format!("deploy failed for project '{project}'"),
                format!("check deploy logs: l8b status --project {project}"),
            ));
        }
        Some(ProjectStatus::Running | ProjectStatus::Completed) => "running",
        Some(ProjectStatus::Stopped) => "stopped",
        _ => "deploying",
    };

    out.ok(&DeployOutcome {
        project_id: project.to_string(),
        status: status_label.to_string(),
        url: url.clone(),
        background,
        duration_secs: duration,
    });

    match status_label {
        "running" => match (&url, background) {
            (Some(u), false) => out.note(&format!("Deployed! {u}")),
            _ => out.note("Deployed! No managed URL (background project)."),
        },
        "stopped" => out.note("Deployed, but the project is now stopped."),
        _ => {
            out.note("Deployment is still in progress.");
            out.note(&format!("Run {} to check status.", format!("l8b status --project {project}").cyan()));
        }
    }
    Ok(())
}

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
    pub env_file: Option<PathBuf>,
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
    out: &Out,
) -> Result<()> {
    let started = std::time::Instant::now();
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
        env_file,
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
        // Reached only when --compose was passed and no compose file was
        // found; with compose == false the outer condition guarantees Some.
        let Some(compose_name) = compose_file else {
            bail!("--compose flag specified but no compose file found in {}", path.display());
        };

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
        let final_status = status::poll_project_status(&client, &server, &project, 120, out.json).await?;
        let url = if effective_background {
            None
        } else {
            let domain = auth::fetch_platform_domain(&client, &server).await;
            Some(auth::project_live_url(&project, &domain))
        };
        finish_deploy(out, &project, final_status, url, effective_background, started)?;
        apply_env_file(&client, &server, &project, env_file.as_ref(), out).await?;
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
            if no_auto_stop { Some(false) } else { None },
            &grant_capability,
        )
        .await?;

        if response.status == ProjectStatus::Deploying {
            // Poll for completion (2 min timeout, non-interactive)
            let final_status = status::poll_project_status(&client, &server, &project, 120, out.json).await?;
            let url = if effective_background {
                None
            } else if let Some(u) = response.url.as_deref().filter(|u| !u.is_empty()) {
                Some(u.to_string())
            } else {
                let domain = auth::fetch_platform_domain(&client, &server).await;
                Some(auth::project_live_url(&project, &domain))
            };
            finish_deploy(out, &project, final_status, url, effective_background, started)?;
            apply_env_file(&client, &server, &project, env_file.as_ref(), out).await?;
        } else {
            let final_status = Some(response.status);
            let url = response.url.clone().filter(|u| !u.is_empty());
            finish_deploy(out, &project, final_status, url, effective_background, started)?;
            apply_env_file(&client, &server, &project, env_file.as_ref(), out).await?;
        }

        // Clean up
        let _ = std::fs::remove_file(&image.path);
    }

    Ok(())
}

/// Push `--env-file` after a deploy (merge); recreate to apply when running.
async fn apply_env_file(
    client: &reqwest::Client,
    server: &str,
    project: &str,
    env_file: Option<&PathBuf>,
    out: &Out,
) -> Result<()> {
    let Some(path) = env_file else { return Ok(()) };
    let raw = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let vars: std::collections::HashMap<String, String> = dotenvy::Iter::new(raw.as_slice())
        .filter_map(|item| item.ok())
        .map(|(k, v)| (k.trim().to_string(), v))
        .collect();
    if vars.is_empty() {
        anyhow::bail!("no environment variables found in {}", path.display());
    }

    let count = vars.len();
    let resp = crate::auth::api_put_json(
        client,
        server,
        &format!("/projects/{project}/env"),
        &serde_json::json!({ "env": vars, "mode": "merge" }),
    )
    .await
    .with_context(|| format!("failed to push env from {}", path.display()))?;
    out.note(&format!("Pushed {count} runtime variable(s) from {}.", path.display()));
    let _ = resp;

    // Apply now when the deploy already reached running.
    let stats = crate::auth::api_get(client, server, &format!("/projects/{project}/stats")).await?;
    if stats["status"].as_str() == Some("running") {
        out.note("Recreating to apply environment…");
        crate::auth::api_post_json(client, server, &format!("/projects/{project}/recreate"), &serde_json::json!({}))
            .await?;
        let final_status = crate::status::poll_project_status(client, server, project, 120, out.json).await?;
        if matches!(final_status, Some(ProjectStatus::Running | ProjectStatus::Completed)) {
            out.note("Environment applied — project is running.");
        } else {
            anyhow::bail!(crate::out::fail(
                "environment pushed but recreate did not reach running",
                format!("check status: l8b status --project {project}")
            ));
        }
    }
    Ok(())
}
