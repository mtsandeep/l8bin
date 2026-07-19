use std::collections::HashSet;

use anyhow::Result;
use colored::Colorize;
use litebin_common::types::ProjectStatus;

/// Poll project status until it reaches a terminal state (running, stopped, error).
/// Returns the final status.
/// If timeout_secs is reached, returns None (still deploying).
pub async fn poll_project_status(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    timeout_secs: u64,
) -> Result<Option<ProjectStatus>> {
    let start = std::time::Instant::now();
    let poll_interval = std::time::Duration::from_secs(3);
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let mut seen_lines: HashSet<String> = HashSet::new();

    loop {
        let resp = client.get(format!("{}/projects/{}/stats", server.trim_end_matches('/'), project_id)).send().await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = r.json().await?;
                let status: ProjectStatus = json["status"]
                    .as_str()
                    .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
                    .unwrap_or(ProjectStatus::Stopped);
                if matches!(
                    status,
                    ProjectStatus::Running | ProjectStatus::Stopped | ProjectStatus::Error | ProjectStatus::Completed
                ) {
                    // Print any remaining new logs before returning
                    fetch_and_print_new_logs(client, server, project_id, &mut seen_lines).await;
                    return Ok(Some(status));
                }
                // Still deploying — show new deploy logs
                fetch_and_print_new_logs(client, server, project_id, &mut seen_lines).await;
            }
            _ => {
                // Non-success — ignore and retry
            }
        }

        if start.elapsed() >= timeout {
            return Ok(None);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Fetch deploy logs and print only lines not yet seen.
async fn fetch_and_print_new_logs(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    seen: &mut HashSet<String>,
) {
    let resp: Result<reqwest::Response, reqwest::Error> =
        client.get(format!("{}/projects/{}/deploy-logs", server.trim_end_matches('/'), project_id)).send().await;

    if let Ok(r) = resp {
        if r.status().is_success() {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                if let Some(lines) = json["lines"].as_array() {
                    for line in lines {
                        if let Some(text) = line.as_str() {
                            if seen.insert(text.to_string()) {
                                println!("    {}", text.dimmed());
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Fetch and display deploy logs for a project.
pub async fn show_deploy_logs(client: &reqwest::Client, server: &str, project_id: &str) -> Result<()> {
    let resp =
        client.get(format!("{}/projects/{}/deploy-logs", server.trim_end_matches('/'), project_id)).send().await?;

    if resp.status().is_success() {
        let json: serde_json::Value = resp.json().await?;
        if let Some(lines) = json["lines"].as_array() {
            for line in lines {
                if let Some(text) = line.as_str() {
                    println!("    {}", text.dimmed());
                }
            }
        }
    }

    Ok(())
}

/// Show detailed project status from the CLI.
pub async fn show_project_status(client: &reqwest::Client, server: &str, project_id: &str) -> Result<()> {
    let resp = client.get(format!("{}/projects/{}", server.trim_end_matches('/'), project_id)).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("Project '{}' not found (HTTP {})", project_id, resp.status());
    }

    let project: serde_json::Value = resp.json().await?;
    let stats = client
        .get(format!("{}/projects/{}/stats", server.trim_end_matches('/'), project_id))
        .send()
        .await
        .ok()
        .filter(|response| response.status().is_success());
    let stats_json = match stats {
        Some(response) => response.json::<serde_json::Value>().await.ok(),
        None => None,
    };

    let status: ProjectStatus = project["status"]
        .as_str()
        .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
        .unwrap_or(ProjectStatus::Stopped);
    let status_str = status.to_string();
    let name = project["name"].as_str().unwrap_or(project_id);
    let is_background = project["is_background"].as_bool().unwrap_or(false);
    let services = stats_json.as_ref().and_then(|stats| stats["services"].as_array());
    let image = project["public_stats"]["image"]
        .as_str()
        .or_else(|| services.and_then(|items| items.first()).and_then(|svc| svc["image"].as_str()));
    let custom_domain = project["custom_domain"].as_str();
    let service_count = project["service_count"].as_u64().unwrap_or(1);
    let url = if is_background {
        None
    } else if let Some(d) = custom_domain.filter(|s| !s.is_empty()) {
        Some(format!("https://{}", d))
    } else {
        let domain = crate::auth::fetch_platform_domain(client, server).await;
        Some(crate::auth::project_live_url(project_id, &domain))
    };

    // Status color
    let status_colored = match &status {
        ProjectStatus::Running => status_str.green().bold(),
        ProjectStatus::Stopped => status_str.dimmed(),
        ProjectStatus::Deploying | ProjectStatus::Importing => status_str.yellow().bold(),
        ProjectStatus::Error => status_str.red().bold(),
        _ => status_str.normal(),
    };

    println!();
    println!("  {} {}", "Project:".dimmed(), name.cyan());
    println!("  {} {}", "ID:".dimmed(), project_id.dimmed());
    println!("  {} {}", "Status:".dimmed(), status_colored);
    if let Some(url) = url {
        println!("  {} {}", "URL:".dimmed(), url.cyan());
    } else {
        println!("  {} {}", "URL:".dimmed(), "No managed URL".dimmed());
    }
    if let Some(img) = image {
        let short = if img.len() > 40 { &img[..37] } else { img };
        println!("  {} {}", "Image:".dimmed(), short.dimmed());
    }
    if service_count > 1 {
        println!("  {} {}", "Services:".dimmed(), format!("{} services", service_count).cyan());
    }

    // Show services
    if let Some(services) = services {
        if !services.is_empty() {
            println!();
            for svc in services {
                let svc_name = svc["service_name"].as_str().unwrap_or("?");
                let svc_status: ProjectStatus = svc["status"]
                    .as_str()
                    .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
                    .unwrap_or(ProjectStatus::Stopped);
                let is_public = svc["is_public"].as_bool().unwrap_or(false);
                let cpu = svc["cpu_percent"].as_f64();
                let mem_mb = svc["memory_mb"].as_u64();

                let svc_status_colored = match &svc_status {
                    ProjectStatus::Running => "running".green(),
                    ProjectStatus::Stopped => "stopped".dimmed(),
                    _ => svc_status.to_string().yellow(),
                };

                let pub_tag = if is_public && !is_background { " (public)".dimmed() } else { "".dimmed() };
                let stats = match (cpu, mem_mb) {
                    (Some(c), Some(m)) => format!("  {} cpu, {}MB mem", format!("{:.1}%", c), m),
                    (Some(c), None) => format!("  {} cpu", format!("{:.1}%", c)),
                    (None, Some(m)) => format!("  {}MB mem", m),
                    _ => String::new(),
                };

                println!("    {} {}{}{}", svc_name.cyan(), svc_status_colored, pub_tag, stats.dimmed());
            }
        }
    }

    // If deploying, show deploy logs
    if status == ProjectStatus::Deploying {
        println!();
        println!("  {} Deploy logs:", "---".dimmed());
        show_deploy_logs(client, server, project_id).await?;
        println!();
        println!("  {} {}", "Tip:".dimmed(), "Run this command again to check for updates.".dimmed());
    }

    Ok(())
}
