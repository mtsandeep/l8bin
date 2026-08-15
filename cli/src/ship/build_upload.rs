use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::HumanBytes;

use crate::auth;

use super::env::{EnvSelectMode, merge_service_env_files, select_env_files};
use super::public_service::inject_public_label;

#[derive(Clone)]
pub(super) struct BuildInfo {
    pub svc_name: String,
    pub build_context: String,
    pub dockerfile: Option<String>,
}

pub(super) fn collect_build_infos(compose: &serde_yaml::Value) -> Vec<BuildInfo> {
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
                let context = build_map.get("context").and_then(|c| c.as_str()).unwrap_or(&name).to_string();
                let df = build_map.get("dockerfile").and_then(|d| d.as_str()).map(|s| s.to_string());
                (context, df)
            }
            _ => (name.clone(), None),
        };
        build_infos.push(BuildInfo { svc_name: name, build_context: ctx, dockerfile });
    }
    build_infos
}

pub(super) fn rewrite_compose_images(
    compose: &serde_yaml::Value,
    resolved_images: &HashMap<String, String>,
) -> Result<String> {
    let mut resolved_compose = compose.clone();
    if let Some(services_map) = resolved_compose.get_mut("services").and_then(|s| s.as_mapping_mut()) {
        for entry in services_map.iter_mut() {
            let svc_name = entry.0.as_str().unwrap_or_default().to_string();
            if let Some(image_id) = resolved_images.get(&svc_name)
                && let Some(svc_map) = entry.1.as_mapping_mut()
            {
                svc_map.remove("build");
                svc_map.insert(
                    serde_yaml::Value::String("image".to_string()),
                    serde_yaml::Value::String(image_id.clone()),
                );
            }
        }
    }
    Ok(serde_yaml::to_string(&resolved_compose)?)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn build_and_upload_services(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    project_dir: &Path,
    build_infos: &[BuildInfo],
    root_env_paths: &[PathBuf],
    node_id: Option<&str>,
    platform: Option<&str>,
    mode: crate::upload::UploadMode,
    ci_mode: bool,
) -> Result<HashMap<String, String>> {
    let mut resolved_images = HashMap::new();

    // Services sharing an identical (context, dockerfile) produce the same image;
    // build + upload once per group and share the resulting image ID.
    for group in group_build_infos(build_infos) {
        let primary = &group[0];
        // dunce::canonicalize strips Windows' verbatim "\\?\" prefix (which Docker
        // rejects), unlike std::fs::canonicalize. No-op on Unix.
        let svc_dir = dunce::canonicalize(project_dir.join(&primary.build_context)).with_context(|| {
            format!("build context '{}' does not exist for service '{}'", primary.build_context, primary.svc_name)
        })?;

        let secret = merge_service_env_files(root_env_paths, &svc_dir);
        let image_tag = format!("{}/{}-{}", crate::config::IMAGE_PREFIX, project_id, primary.svc_name);
        let names: Vec<&str> = group.iter().map(|g| g.svc_name.as_str()).collect();
        let label =
            if group.len() > 1 { format!("{} (shared image)", names.join(", ")) } else { primary.svc_name.clone() };
        println!("    {} {} ({})", "→".dimmed(), label.cyan(), svc_dir.display());

        let saved_image =
            crate::build::build_project(&svc_dir, primary.dockerfile.as_deref(), &image_tag, secret, ci_mode, platform)
                .await?;
        println!("    {} {} — {}", "  ✓".green(), names.join(", "), HumanBytes(saved_image.compressed_size));

        let image_id = crate::upload::upload_image(
            client,
            server,
            project_id,
            Path::new(&saved_image.path),
            &saved_image.image_id,
            node_id,
            mode,
            ci_mode,
        )
        .await?;
        let _ = std::fs::remove_file(&saved_image.path);

        for info in &group {
            resolved_images.insert(info.svc_name.clone(), image_id.clone());
        }
    }

    Ok(resolved_images)
}

/// Group `BuildInfo`s whose `(build_context, dockerfile)` are identical, preserving
/// first-seen order. Each group yields one image build + upload shared by its members.
fn group_build_infos(build_infos: &[BuildInfo]) -> Vec<Vec<&BuildInfo>> {
    let mut keys: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<&BuildInfo>> = Vec::new();
    for info in build_infos {
        let key = format!("{}::{}", info.build_context, info.dockerfile.as_deref().unwrap_or(""));
        if let Some(idx) = keys.iter().position(|k| *k == key) {
            groups[idx].push(info);
        } else {
            keys.push(key);
            groups.push(vec![info]);
        }
    }
    groups
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn submit_compose(
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
        form = form.text("stage_only", "true");
    }
    if !grant_capabilities.is_empty() {
        form = form.text("grant_capabilities", grant_capabilities.join(","));
    }

    auth::session_post_multipart(client, server, "/deploy/compose", form).await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_compose_deployment(
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
    mode: crate::upload::UploadMode,
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
        mode,
        ci_mode,
    )
    .await?;

    let mut resolved_yaml = rewrite_compose_images(compose, &resolved_images)?;
    if let Some(service_name) = selected_public {
        resolved_yaml = inject_public_label(&resolved_yaml, service_name)?;
    }
    Ok(resolved_yaml)
}

#[cfg(test)]
mod tests {
    use super::{BuildInfo, group_build_infos};

    fn info(name: &str, ctx: &str, df: Option<&str>) -> BuildInfo {
        BuildInfo { svc_name: name.to_string(), build_context: ctx.to_string(), dockerfile: df.map(str::to_string) }
    }

    #[test]
    fn groups_identical_context_and_dockerfile() {
        let infos = vec![
            info("a", ".", Some("Dockerfile.init")),
            info("b", ".", Some("Dockerfile.init")),
            info("c", ".", Some("Dockerfile")),
            info("d", ".", Some("Dockerfile.init")),
            info("e", "web", Some("Dockerfile")),
        ];
        let groups = group_build_infos(&infos);

        // names that ended up sharing each group, in first-seen order
        let grouped: Vec<Vec<&str>> = groups.iter().map(|g| g.iter().map(|i| i.svc_name.as_str()).collect()).collect();
        assert_eq!(grouped, vec![vec!["a", "b", "d"], vec!["c"], vec!["e"]]);
    }
}
