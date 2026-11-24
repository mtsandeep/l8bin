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
    api_custom_domain: Option<&str>,
) -> String {
    // Custom domain first; older servers don't send the fact, so look it up.
    if let Some(d) = api_custom_domain.map(str::trim).filter(|d| !d.is_empty()) {
        return format!("https://{d}");
    }
    if let Some(url) = crate::auth::custom_domain_url(client, server, project_id).await {
        return url;
    }
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

    let detected = std::fs::read_to_string(project_dir.join("Dockerfile"))
        .map(|content| litebin_common::types::detect_exposed_ports(&content))
        .unwrap_or_default();
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

/// All compose file candidates: the well-known names first (preference
/// order), then `docker-compose.*` / `compose.*` variants (override files
/// excluded — they merge, they are not standalone stacks).
pub fn detect_compose_files(project_dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = litebin_common::types::COMPOSE_FILE_NAMES
        .iter()
        .filter(|p| project_dir.join(p).exists())
        .map(|s| s.to_string())
        .collect();
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        let mut variants: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| {
                (n.starts_with("docker-compose.") || n.starts_with("compose."))
                    && (n.ends_with(".yml") || n.ends_with(".yaml"))
                    && !n.starts_with("docker-compose.override.")
                    && !files.contains(n)
            })
            .collect();
        variants.sort();
        files.extend(variants);
    }
    files
}

/// Which compose file a deploy uses: an explicit override, else the stored
/// `compose` choice, else the first well-known name.
pub fn resolve_compose_files(project_dir: &Path, compose_override: Option<&[String]>) -> Option<Vec<String>> {
    let overrides: Vec<String> =
        compose_override.map(|v| v.iter().filter(|s| !s.is_empty()).cloned().collect()).unwrap_or_default();
    if !overrides.is_empty() {
        return Some(overrides);
    }
    if let Some(stored) = crate::project_config::load_merged(project_dir).compose {
        let existing: Vec<String> = stored.into_iter().filter(|f| project_dir.join(f).exists()).collect();
        if !existing.is_empty() {
            return Some(existing);
        }
    }
    detect_compose_file(project_dir).map(|f| vec![f.to_string()])
}

/// Compose merge rules: mappings merge recursively, anything else in the
/// later file replaces the earlier value (ports, lists, scalars).
fn merge_compose_value(base: &mut serde_yaml::Value, over: &serde_yaml::Value) {
    match (base.as_mapping_mut(), over.as_mapping()) {
        (Some(b), Some(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge_compose_value(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        _ => *base = over.clone(),
    }
}

/// Read and merge compose files in order into one document.
pub(super) fn load_compose_files(project_dir: &Path, names: &[String]) -> Result<serde_yaml::Value> {
    let label = if names.len() == 1 { names[0].clone() } else { format!("{} (merged in order)", names.join(" + ")) };
    println!("  {} Found {} — deploying as multi-service", "🐳".dimmed(), label.cyan());
    let mut merged: Option<serde_yaml::Value> = None;
    for name in names {
        let raw = std::fs::read_to_string(project_dir.join(name)).with_context(|| format!("failed to read {name}"))?;
        let value: serde_yaml::Value = serde_yaml::from_str(&raw).with_context(|| format!("failed to parse {name}"))?;
        match &mut merged {
            None => merged = Some(value),
            Some(m) => merge_compose_value(m, &value),
        }
    }
    merged.ok_or_else(|| anyhow::anyhow!("no compose files given"))
}

/// Resolve the target Docker platform string from a list of nodes.
pub fn resolve_platform(nodes: &[crate::auth::NodeInfo], node_id: Option<&str>) -> Option<String> {
    let arch = match node_id {
        Some(id) => nodes.iter().find(|n| n.id == id).and_then(|n| n.architecture.as_deref()),
        // No node choice means the deploy lands on the master.
        None => {
            nodes.iter().find(|n| n.id == "local").or_else(|| nodes.first()).and_then(|n| n.architecture.as_deref())
        }
    };
    arch.map(|a| match a {
        "aarch64" => "linux/arm64".to_string(),
        "x86_64" => "linux/amd64".to_string(),
        other => format!("linux/{}", other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NodeInfo;
    use litebin_common::types::NodeStatus;

    #[test]
    fn compose_candidates_and_resolution() {
        let dir = std::env::temp_dir().join(format!("l8b-compose-{}-{}", "detect", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["docker-compose.yml", "docker-compose.staging.yml", "docker-compose.override.yml", "README.md"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }

        let candidates = detect_compose_files(&dir);
        assert_eq!(candidates, vec!["docker-compose.yml", "docker-compose.staging.yml"]);

        // Default: the well-known name. Override: the flag(s), then the stored choice.
        let well_known = vec!["docker-compose.yml".to_string()];
        assert_eq!(resolve_compose_files(&dir, None), Some(well_known));
        let pick = vec!["docker-compose.staging.yml".to_string()];
        assert_eq!(resolve_compose_files(&dir, Some(&pick)), Some(pick.clone()));
        crate::project_config::set_compose(&dir, "docker-compose.staging.yml").unwrap();
        assert_eq!(resolve_compose_files(&dir, None), Some(pick));

        // Merged files: mappings deep-merge, later lists/scalars replace.
        std::fs::write(
            dir.join("docker-compose.base.yml"),
            "services:
  api:
    image: old
    environment:
      A: 1
      B: 2
    ports: [\"3000\"]
  worker:
    image: w
",
        )
        .unwrap();
        std::fs::write(
            dir.join("docker-compose.over.yml"),
            "services:
  api:
    image: new
    environment:
      B: 9
    ports: [\"3000:3000\"]
",
        )
        .unwrap();
        let merged =
            load_compose_files(&dir, &["docker-compose.base.yml".into(), "docker-compose.over.yml".into()]).unwrap();
        let api = &merged["services"]["api"];
        assert_eq!(api["image"].as_str(), Some("new"));
        assert_eq!(api["environment"]["A"].as_i64(), Some(1));
        assert_eq!(api["environment"]["B"].as_i64(), Some(9));
        assert_eq!(api["ports"].as_sequence().unwrap().len(), 1);
        assert_eq!(merged["services"]["worker"]["image"].as_str(), Some("w"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn node(id: &str, arch: Option<&str>, recommended: bool) -> NodeInfo {
        NodeInfo {
            id: id.to_string(),
            name: id.to_string(),
            status: NodeStatus::Online,
            architecture: arch.map(str::to_string),
            recommended: Some(recommended),
            public_ip: None,
        }
    }

    #[test]
    fn platform_follows_explicit_node() {
        let nodes = vec![node("local", Some("x86_64"), false), node("armbox", Some("aarch64"), false)];
        assert_eq!(resolve_platform(&nodes, Some("armbox")).as_deref(), Some("linux/arm64"));
    }

    #[test]
    fn unspecified_node_builds_for_master_not_recommended() {
        let nodes = vec![node("armbox", Some("aarch64"), true), node("local", Some("x86_64"), false)];
        assert_eq!(resolve_platform(&nodes, None).as_deref(), Some("linux/amd64"));
    }
}
