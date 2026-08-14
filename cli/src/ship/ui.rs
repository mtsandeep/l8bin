use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::Input;
use indicatif::{ProgressBar, ProgressStyle};

use super::flow::ProjectInfo;

pub(super) fn spinner(template: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template(template).unwrap());
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb
}

pub(super) fn stop_buildkit() {
    println!("  🧹 Stopping BuildKit...");
    let _ = std::process::Command::new("docker")
        .args(["stop", "buildkit"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub(super) async fn resolve_live_url(
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
    let domain = crate::auth::fetch_platform_domain(client, server).await;
    crate::auth::project_live_url(project_id, &domain)
}

pub(super) fn show_env_path(server: &str, project_id: &str, node_id: Option<&str>) {
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
    println!("     {}", "(default install path; if custom -InstallDir was used, prepend that path instead)".dimmed());
    if node_id.is_some() && node_id != Some("local") {
        println!("     {}", "Edit this file on the selected agent node before continuing.".dimmed());
    }
}

pub(super) fn project_is_staged(project: &ProjectInfo) -> bool {
    project.is_staged || project.public_stats.as_ref().map(|ps| !ps.image.is_empty()).unwrap_or(false)
}

pub(super) fn short_image(image: &str) -> String {
    let hash = image.strip_prefix("sha256:").unwrap_or(image);
    if hash.len() > 12 { hash[..12].to_string() } else { hash.to_string() }
}

pub(super) fn print_live_url(url: &str) {
    println!();
    println!("  {} Live at: {}", "🌐".dimmed(), url.green().bold());
    println!();
}

pub(super) fn print_no_managed_url() {
    println!();
    println!("  {} No managed URL (background project)", "⚙".dimmed());
    println!();
}

pub(super) fn resolve_app_port(project_dir: &Path, port_override: Option<u16>) -> Result<u16> {
    if let Some(p) = port_override {
        return Ok(p);
    }
    if super::detect_compose_file(project_dir).is_some() {
        return Ok(0);
    }

    let detected = detect_exposed_ports(project_dir);
    match detected.as_slice() {
        [single] => {
            println!("  {} Detected exposed port {}", "::".dimmed(), single);
            Ok(*single)
        }
        [first, rest @ ..] => {
            let all: Vec<String> = std::iter::once(first).chain(rest.iter()).map(|p| p.to_string()).collect();
            println!("  {} Detected ports: {} — using {}", "::".dimmed(), all.join(", "), first);
            Ok(*first)
        }
        [] => {
            let input: String = Input::new().with_prompt("App port").default("3000".to_string()).interact_text()?;
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

pub(super) fn load_compose(project_dir: &Path, compose_name: &str) -> Result<serde_yaml::Value> {
    println!("  {} Found {} — deploying as multi-service", "🐳".dimmed(), compose_name.cyan());

    let compose_yaml = std::fs::read_to_string(project_dir.join(compose_name))
        .with_context(|| format!("failed to read {}", compose_name))?;
    serde_yaml::from_str(&compose_yaml).with_context(|| "failed to parse compose YAML")
}

pub(super) fn print_compose_build_summary(compose: &serde_yaml::Value, build_infos: &[super::build_upload::BuildInfo]) {
    if build_infos.is_empty() {
        return;
    }

    let total_services = compose.get("services").and_then(|s| s.as_mapping()).map(|m| m.len()).unwrap_or(0);
    let pull_count = total_services.saturating_sub(build_infos.len());
    let pull_info = if pull_count > 0 {
        format!(" ({} pre-built will be pulled by orchestrator)", pull_count)
    } else {
        String::new()
    };
    println!("  {} Found {} services — building {}{}", "🐳".dimmed(), total_services, build_infos.len(), pull_info);
}

pub(super) fn print_building_services(build_infos: &[super::build_upload::BuildInfo]) {
    if !build_infos.is_empty() {
        println!("  {} Building {} service(s)...", "🔨".dimmed(), build_infos.len());
    }
}

/// Detect compose file in the given directory. Returns the filename or None.
pub fn detect_compose_file(project_dir: &Path) -> Option<&'static str> {
    litebin_common::types::find_compose_file(project_dir)
}

/// Resolve the target Docker platform string from a list of nodes.
pub fn resolve_platform(nodes: &[crate::auth::NodeInfo], node_id: Option<&str>) -> Option<String> {
    let arch = match node_id {
        Some(id) => nodes.iter().find(|n| n.id == id).and_then(|n| n.architecture.as_deref()),
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
