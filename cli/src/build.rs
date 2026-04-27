use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Detected project info from `railpack info`.
pub struct ProjectInfo {
    pub project_type: String,
    pub package: String,
}

/// Detect project type by running `railpack info`.
pub fn detect_project(project_dir: &Path) -> Result<ProjectInfo> {
    if project_dir.join("Dockerfile").exists() {
        return Ok(ProjectInfo {
            project_type: "Dockerfile".to_string(),
            package: String::new(),
        });
    }

    let project_dir_str = project_dir.to_string_lossy().to_string();

    if cfg!(target_os = "windows") {
        let mut cmd = Command::new("docker");
        cmd.args([
            "run", "--rm",
            "-v", &format!("{}:/app", project_dir_str),
        ]);

        // Mask gitignored directories to speed up the mount
        for dir in gitignored_dirs(project_dir) {
            cmd.args(["--tmpfs", &format!("/app/{}", dir)]);
        }

        cmd.args([
            "--entrypoint", "sh",
            crate::config::RAILPACK_IMAGE,
            "-c", "railpack info /app --format json 2>/dev/null",
        ]);
        cmd.env("MSYS_NO_PATHCONV", "1");
        
        let output = cmd.output();

        match output {
            Ok(out) if out.status.success() => parse_info_output(&String::from_utf8_lossy(&out.stdout)),
            _ => Ok(ProjectInfo {
                project_type: "Unknown".to_string(),
                package: String::new(),
            }),
        }
    } else {
        let bin_path = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(crate::config::APP_DIR)
            .join("bin")
            .join("railpack");

        if !bin_path.exists() {
            return Ok(ProjectInfo {
                project_type: "Unknown".to_string(),
                package: String::new(),
            });
        }

        let output = Command::new(&bin_path)
            .args(["info", &project_dir_str, "--format", "json"])
            .output();

        match output {
            Ok(out) if out.status.success() => parse_info_output(&String::from_utf8_lossy(&out.stdout)),
            _ => Ok(ProjectInfo {
                project_type: "Unknown".to_string(),
                package: String::new(),
            }),
        }
    }
}

fn parse_info_output(json: &str) -> Result<ProjectInfo> {
    let info: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|_| serde_json::Value::Null);

    // detectedProviders: e.g. ["staticfile"], ["node"], ["python"]
    let project_type = info["detectedProviders"]
        .as_array()
        .and_then(|arr| {
            let names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            if names.is_empty() { None } else { Some(names.join(", ")) }
        })
        .unwrap_or_else(|| "Unknown".to_string());

    // resolvedPackages: e.g. {"caddy": {"name": "caddy", "resolvedVersion": "2.11.2"}}
    let mut packages = Vec::new();
    if let Some(obj) = info["resolvedPackages"].as_object() {
        for (_key, pkg) in obj {
            let name = pkg["name"].as_str().unwrap_or("");
            let version = pkg["resolvedVersion"].as_str().unwrap_or("");
            if !name.is_empty() && !version.is_empty() {
                packages.push(format!("{}@{}", name, version));
            }
        }
    }
    let package = packages.join(", ");

    Ok(ProjectInfo { project_type, package })
}

/// Result of building a Docker image.
pub struct SavedImage {
    pub path: String,
    pub image_id: String,
    pub image_size: u64,
    pub compressed_size: u64,
}

/// Detect build strategy and produce a Docker image tar file.
/// When `ci_mode` is true, build logs are suppressed to prevent secret leakage.
pub async fn build_project(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    secret: Vec<std::path::PathBuf>,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    build_project_inner(project_dir, dockerfile, image_tag, secret, true, ci_mode, platform).await
}

async fn build_project_inner(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    secret: Vec<std::path::PathBuf>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let has_dockerfile = if let Some(df) = dockerfile {
        project_dir.join(df).exists()
    } else {
        project_dir.join("Dockerfile").exists()
    };

    // Context guard manages .dockerignore always, and .env injection when secrets are provided
    let _ctx_guard = BuildContextGuard::new(project_dir, dockerfile.unwrap_or("Dockerfile"), secret)?;

    let result = if has_dockerfile {
        build_with_docker(project_dir, dockerfile, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    } else if cfg!(target_os = "windows") {
        check_docker_available()?;
        build_with_railpack_docker(project_dir, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    } else {
        build_with_railpack_native(project_dir, image_tag, Some(&_ctx_guard), quiet, ci_mode, platform).await
    };

    // Temp .dockerignore and .env are cleaned up when _ctx_guard drops
    result
}

async fn build_with_docker(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    _ctx_guard: Option<&BuildContextGuard>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    if !quiet && !ci_mode {
        println!("Building with Docker...");
    }

    let mut cmd = Command::new("docker");
    cmd.arg("build");
    if let Some(p) = platform {
        cmd.args(["--platform", p]);
    }
    if let Some(df) = dockerfile {
        cmd.args(["-f", df]);
    }
    cmd.args(["-t", image_tag, "."]);
    cmd.current_dir(project_dir);

    let spinner = if ci_mode { None } else if quiet { Some(create_build_spinner()) } else { None };

    if quiet || ci_mode {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd
            .output()
            .context("failed to run docker build. Is Docker installed?")?;

        if !output.status.success() {
            if let Some(s) = &spinner { s.finish_and_clear(); }
            if !ci_mode {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("{}", stdout);
                eprintln!("{}", stderr);
            }
            anyhow::bail!("docker build failed with exit code {:?}", output.status.code());
        }

        if let Some(s) = &spinner {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(summary) = parse_docker_summary(&stdout) {
                s.suspend(|| println!("  {}", summary));
            }
        }
    } else {
        let status = cmd
            .status()
            .context("failed to run docker build. Is Docker installed?")?;
        if !status.success() {
            anyhow::bail!("docker build failed with exit code {:?}", status.code());
        }
    }

    let result = save_tar(image_tag)?;
    if let Some(s) = &spinner { s.finish_and_clear(); }
    Ok(result)
}

// Railpack via Docker (Windows)

const RAILPACK_IMAGE: &str = crate::config::RAILPACK_IMAGE;

fn ensure_railpack_image(railpack_tag: &str, mise_version: &str, quiet: bool, ci_mode: bool) -> Result<()> {
    let inspect = Command::new("docker")
        .args(["image", "inspect", "--format", "{{index .Config.Labels \"version\"}}", RAILPACK_IMAGE])
        .output()?;
    let current_label = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
    let expected_label = format!("rp={} mise={}", railpack_tag, mise_version);

    if inspect.status.success() && current_label == expected_label {
        if !quiet && !ci_mode { println!("  {} Railpack image ready", "✔".green()); }
        return Ok(());
    }

    // Remove stale image
    let _ = Command::new("docker")
        .args(["rmi", "-f", RAILPACK_IMAGE])
        .output();

    if !quiet && !ci_mode { println!("  🔨 Building Railpack image..."); }

    let tmp = std::env::temp_dir().join("l8b-railpack-image");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let dockerfile = format!(
        r#"FROM alpine:3.23
LABEL version="{label}"
RUN apk add --no-cache ca-certificates curl tar docker-cli
ARG RP_TAG={rp}
ARG MISE_VER={mise}
RUN mkdir -p /tmp/rp && cd /tmp/rp && \
    curl -sL "{rp_base}/${{RP_TAG}}/railpack-${{RP_TAG}}-x86_64-unknown-linux-musl.tar.gz" \
    | tar xz && \
    find /tmp/rp -name railpack -type f -exec mv {{}} /usr/local/bin/railpack \; && \
    chmod +x /usr/local/bin/railpack && rm -rf /tmp/rp
RUN mkdir -p /tmp/railpack/mise && \
    mkdir -p /tmp/mise-extract && cd /tmp/mise-extract && \
    curl -sL "{mise_base}/v${{MISE_VER}}/mise-v${{MISE_VER}}-linux-x64-musl.tar.gz" \
    | tar xz && \
    mv mise/bin/mise "/tmp/railpack/mise/mise-${{MISE_VER}}" && \
    chmod +x "/tmp/railpack/mise/mise-${{MISE_VER}}" && \
    rm -rf /tmp/mise-extract
ENTRYPOINT ["railpack"]
"#,
        rp = railpack_tag,
        mise = mise_version,
        label = format!("rp={} mise={}", railpack_tag, mise_version),
        rp_base = crate::config::RAILPACK_RELEASE_BASE,
        mise_base = crate::config::MISE_RELEASE_BASE,
    );

    std::fs::write(tmp.join("Dockerfile"), &dockerfile)?;

    let mut cmd = Command::new("docker");
    cmd.args(["build", "-t", RAILPACK_IMAGE, "."])
        .current_dir(&tmp);

    let status = if ci_mode {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = cmd.output().context("failed to build Railpack image")?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&tmp);
            anyhow::bail!("failed to build Railpack Docker image");
        }
        output.status
    } else {
        cmd.status().context("failed to build Railpack image")?
    };

    let _ = std::fs::remove_dir_all(&tmp);

    if !status.success() {
        anyhow::bail!("failed to build Railpack Docker image");
    }

    if !quiet && !ci_mode { println!("  {} Railpack image built", "✔".green()); }
    Ok(())
}

async fn build_with_railpack_docker(
    project_dir: &Path,
    image_tag: &str,
    _ctx_guard: Option<&BuildContextGuard>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    
    let railpack_spinner = if !ci_mode {
        let s = indicatif::ProgressBar::new_spinner();
        s.set_style(indicatif::ProgressStyle::default_spinner().template("  ⚙️  {spinner} {msg}").unwrap());
        s.enable_steady_tick(std::time::Duration::from_millis(100));
        s.set_message("Loading Railpack...");
        Some(s)
    } else {
        None
    };

    let resp: serde_json::Value = client
        .get(crate::config::RAILPACK_RELEASE_URL)
        .header("User-Agent", "l8b-cli")
        .send()
        .await
        .context("failed to fetch Railpack release info")?
        .json()
        .await
        .context("failed to parse Railpack release info")?;
    let railpack_tag = resp["tag_name"].as_str().unwrap_or("v0.23.0");
    let mise_version = crate::mise::fetch_mise_version(railpack_tag).await;

    ensure_railpack_image(railpack_tag, &mise_version, quiet, ci_mode)?;
    ensure_buildkit(quiet, ci_mode)?;

    if let Some(s) = &railpack_spinner {
        s.finish_and_clear();
    }

    let spinner = if ci_mode { None } else if quiet { Some(create_build_spinner()) } else { None };

    let project_dir_str = project_dir.to_string_lossy().to_string();
    let max_retries = crate::config::MAX_RETRIES;
    let mut last_output = None;

    for attempt in 1..=max_retries {
        let mut cmd = Command::new("docker");
        cmd.args([
            "run", "--rm",
            "-v", &format!("{}:/app", project_dir_str),
            "-v", "/var/run/docker.sock:/var/run/docker.sock",
            "-e", "BUILDKIT_HOST=docker-container://buildkit",
        ]);

        // Mask gitignored directories so the bind mount doesn't leak
        // node_modules, .git, etc. into the build context.
        for dir in gitignored_dirs(project_dir) {
            cmd.args(["--tmpfs", &format!("/app/{}", dir)]);
        }

        cmd.arg(RAILPACK_IMAGE);

        let args = vec!["build", "--name", image_tag];
        let mut rp_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        if let Some(p) = platform {
            rp_args.push("--platform".to_string());
            rp_args.push(p.to_string());
        }

        rp_args.push("/app".to_string());
        cmd.args(rp_args);

        cmd.env("MSYS_NO_PATHCONV", "1");
        if attempt == max_retries {
            cmd.arg("--verbose");
        }

        if quiet || ci_mode {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            let output = cmd
                .output()
                .context("failed to run Railpack in Docker. Is Docker Desktop running?")?;

            if output.status.success() {
                let result = save_tar(image_tag)?;
                if let Some(s) = &spinner { s.finish_and_clear(); }
                return Ok(result);
            }

            last_output = Some((
                String::from_utf8_lossy(&output.stderr).to_string(),
                String::from_utf8_lossy(&output.stdout).to_string(),
            ));
        } else {
            let status = cmd
                .status()
                .context("failed to run Railpack in Docker. Is Docker Desktop running?")?;

            if status.success() {
                return save_tar(image_tag);
            }
        }

        if attempt < max_retries && !ci_mode {
            println!("  {} Build failed (attempt {}/{}), retrying...", "!".yellow(), attempt, max_retries);
        }
    }

    if let Some(s) = &spinner { s.finish_and_clear(); }
    if let Some((stderr, stdout)) = last_output {
        if !ci_mode {
            eprintln!("{}", stdout);
            eprintln!("{}", stderr);
        }
    }
    anyhow::bail!("railpack build failed after {} attempts", max_retries);
}

// Railpack native (Linux / macOS)

async fn build_with_railpack_native(
    project_dir: &Path,
    image_tag: &str,
    _ctx_guard: Option<&BuildContextGuard>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let (railpack_bin, railpack_tag) = crate::railpack::ensure_railpack(ci_mode).await?;
    crate::mise::ensure_mise_for_railpack(&railpack_tag, ci_mode).await?;

    let buildkit_host = ensure_buildkit(quiet, ci_mode)?;

    let spinner = if ci_mode { None } else if quiet { Some(create_build_spinner()) } else { None };

    if !quiet && !ci_mode {
        println!("No Dockerfile found. Building with Railpack...");
    }

    let max_retries = crate::config::MAX_RETRIES;
    let mut last_output = None;

    for attempt in 1..=max_retries {
        let mut cmd = Command::new(&railpack_bin);
        cmd.args(["build", "--name", image_tag]);
        if let Some(p) = platform {
            cmd.args(["--platform", p]);
        }
        cmd.arg(".");
        if attempt == max_retries {
            cmd.arg("--verbose");
        }
        cmd.env("BUILDKIT_HOST", &buildkit_host);
        cmd.current_dir(project_dir);

        if quiet || ci_mode {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

            let output = cmd
                .output()
                .context("failed to run railpack build")?;

            if output.status.success() {
                let result = save_tar(image_tag)?;
                if let Some(s) = &spinner { s.finish_and_clear(); }
                if !ci_mode {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if let Some(summary) = parse_railpack_summary(&stdout) {
                        println!("  {}", summary);
                    }
                }
                return Ok(result);
            }

            last_output = Some((
                String::from_utf8_lossy(&output.stderr).to_string(),
                String::from_utf8_lossy(&output.stdout).to_string(),
            ));
        } else {
            let status = cmd
                .status()
                .context("failed to run railpack build")?;

            if status.success() {
                return save_tar(image_tag);
            }
        }

        if attempt < max_retries && !ci_mode {
            println!(
                "  {} Build failed (attempt {}/{}), retrying...",
                "!".yellow(),
                attempt,
                max_retries
            );
        }
    }

    if let Some(s) = &spinner { s.finish_and_clear(); }
    if let Some((stderr, stdout)) = last_output {
        if !ci_mode {
            eprintln!("{}", stdout);
            eprintln!("{}", stderr);
        }
    }
    anyhow::bail!("railpack build failed after {} attempts", max_retries)
}

/// A guard that manages the temporary build context (.env and .dockerignore)
pub struct BuildContextGuard {
    _project_dir: PathBuf,
    env_path: PathBuf,
    env_backup: Option<PathBuf>,
    pub ignore_path: Option<PathBuf>,
}

impl BuildContextGuard {
    pub fn new(project_dir: &Path, dockerfile_name: &str, secret_files: Vec<PathBuf>) -> Result<Self> {
        let has_secrets = !secret_files.is_empty();

        // 1. Merge secrets into .env (only when secrets are provided)
        let env_path = project_dir.join(".env");
        let mut env_backup = None;

        if has_secrets {
            let mut merged_env = std::collections::BTreeMap::new();
            for file in secret_files {
                if let Ok(content) = std::fs::read_to_string(&file) {
                    for line in content.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') { continue; }
                        if let Some((key, val)) = line.split_once('=') {
                            merged_env.insert(key.trim().to_string(), val.trim().to_string());
                        }
                    }
                }
            }

            let merged_content = merged_env.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("\n");

            // Backup existing .env if present
            if env_path.exists() {
                let backup_path = project_dir.join(format!(".env.l8b_backup_{}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs()));
                std::fs::rename(&env_path, &backup_path)?;
                env_backup = Some(backup_path);
            }
            std::fs::write(&env_path, merged_content)?;
        }

        // 2. Generate <Dockerfile>.dockerignore via naming convention (No Touch strategy)
        // Docker automatically looks for [Dockerfile-name].dockerignore
        let mut ignore_content = String::new();

        // Load existing ignore patterns
        let existing_ignore = project_dir.join(".dockerignore");
        let gitignore = project_dir.join(".gitignore");

        let mut base_ignore = String::new();
        if existing_ignore.exists() {
            base_ignore = std::fs::read_to_string(&existing_ignore)?;
        } else if gitignore.exists() {
            base_ignore = std::fs::read_to_string(&gitignore)?;
        }

        ignore_content.push_str(&base_ignore);
        // Only force-include .env when we injected secrets (otherwise respect the ignore patterns)
        if has_secrets {
            ignore_content.push_str("\n!.env*\n");
        }

        let dockerfile_basename = Path::new(dockerfile_name).file_name().unwrap_or_default().to_string_lossy();
        let target_ignore = project_dir.join(format!("{}.dockerignore", dockerfile_basename));

        // Only write if there's actual content or we need the !.env* override
        if !ignore_content.trim().is_empty() || has_secrets {
            std::fs::write(&target_ignore, &ignore_content)?;
        }

        Ok(Self {
            _project_dir: project_dir.to_path_buf(),
            env_path,
            env_backup,
            ignore_path: if !ignore_content.trim().is_empty() || has_secrets { Some(target_ignore) } else { None },
        })
    }
}

impl Drop for BuildContextGuard {
    fn drop(&mut self) {
        // Cleanup injected .env
        let _ = std::fs::remove_file(&self.env_path);
        if let Some(ref backup) = self.env_backup {
            let _ = std::fs::rename(backup, &self.env_path);
        }

        // Cleanup temporary ignore file
        if let Some(ref ip) = self.ignore_path {
            let _ = std::fs::remove_file(ip);
        }
    }
}


// Shared helpers

/// Clean up leftover build artifacts from interrupted builds.
///
/// Scans for `.env.l8b_backup_*` files and `<Dockerfile>.dockerignore` temp files,
/// prompts the user before restoring .env if both files have content.
pub fn cleanup_build_artifacts(dir: &Path) -> Result<()> {
    let mut cleaned = 0usize;
    let mut skipped = 0usize;

    // 1. Find and restore .env backups
    let backups: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".env.l8b_backup_")
        })
        .collect();

    if !backups.is_empty() {
        println!("  Found {} backup file(s):", backups.len());

        for entry in &backups {
            let backup_path = entry.path();
            let env_path = dir.join(".env");
            let name = backup_path.file_name().unwrap_or_default().to_string_lossy();

            let backup_content = std::fs::read_to_string(&backup_path).unwrap_or_default();
            let backup_has_values = backup_content.lines()
                .any(|l| {
                    let l = l.trim();
                    !l.is_empty() && !l.starts_with('#') && l.contains('=')
                });

            let env_exists = env_path.exists();
            let env_has_values = if env_exists {
                let env_content = std::fs::read_to_string(&env_path).unwrap_or_default();
                env_content.lines()
                    .any(|l| {
                        let l = l.trim();
                        !l.is_empty() && !l.starts_with('#') && l.contains('=')
                    })
            } else {
                false
            };

            let should_restore = if env_exists && env_has_values && !backup_has_values {
                // Backup is empty but current .env has values — risky
                println!("    {} {} — backup is empty but current .env has content", "!".yellow(), name);
                println!("       Skipping to avoid data loss.");
                false
            } else if env_exists && env_has_values && backup_has_values {
                // Both have values — confirm
                println!("    {} {} — both backup and current .env have content", "!".yellow(), name);
                let confirm = dialoguer::Confirm::new()
                    .with_prompt("       Replace current .env with backup?")
                    .default(false)
                    .interact()?;
                confirm
            } else {
                // Safe to restore (no current .env, or current is empty/placeholder)
                true
            };

            if should_restore {
                if env_exists {
                    let _ = std::fs::remove_file(&env_path);
                }
                match std::fs::rename(&backup_path, &env_path) {
                    Ok(()) => {
                        println!("    {} Restored {} -> .env", "✔".green(), name);
                        cleaned += 1;
                    }
                    Err(e) => {
                        println!("    {} Failed to restore {}: {}", "✗".red(), name, e);
                        skipped += 1;
                    }
                }
            } else {
                skipped += 1;
            }
        }
    }

    // 2. Find and remove temp dockerignore files
    let dockerignores: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            name_str.ends_with(".dockerignore") && name_str != ".dockerignore"
        })
        .collect();

    for entry in &dockerignores {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("    {} Removed {}", "✔".green(), name);
                cleaned += 1;
            }
            Err(e) => {
                println!("    {} Failed to remove {}: {}", "✗".red(), name, e);
                skipped += 1;
            }
        }
    }

    if cleaned == 0 && skipped == 0 {
        println!("  No build artifacts found. Directory is clean.");
    } else {
        println!();
        println!("  Cleaned: {}, Skipped: {}", cleaned.to_string().green(), skipped);
    }

    Ok(())
}

/// Return directory names to mask in the Docker build context on Windows.
/// Reads `.dockerignore` if present, otherwise falls back to `.gitignore`.
/// Only returns top-level directory names that actually exist on disk.
pub(crate) fn gitignored_dirs(project_dir: &Path) -> Vec<String> {
    let mut dirs = std::collections::HashSet::new();
    
    // Always try to mask these common suspects if they exist
    let defaults = vec![".git", "node_modules", ".next", ".vercel", "build", "dist", "target"];
    for d in defaults {
        if project_dir.join(d).is_dir() {
            dirs.insert(d.to_string());
        }
    }

    let ignore_file = if project_dir.join(".dockerignore").exists() {
        project_dir.join(".dockerignore")
    } else {
        project_dir.join(".gitignore")
    };

    if let Ok(content) = std::fs::read_to_string(&ignore_file) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            
            // Clean the pattern
            let pattern = line.trim_start_matches('/');
            
            // On Windows, we can only reliably mask top-level directories via --tmpfs.
            // If the pattern is complex (has * or / in the middle), we skip it 
            // and let the internal .dockerignore handle it.
            
            // 1. If it's a simple directory name (e.g. "node_modules" or ".next/")
            let clean_pattern = pattern.trim_end_matches('/');
            if !clean_pattern.contains('/') && !clean_pattern.contains('*') {
                if project_dir.join(clean_pattern).is_dir() {
                    dirs.insert(clean_pattern.to_string());
                }
            }
        }
    }

    dirs.into_iter().collect()
}

fn create_build_spinner() -> indicatif::ProgressBar {
    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .template("  🔨 {spinner} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message("Building image...");
    spinner
}

fn check_docker_available() -> Result<()> {
    let output = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .context("failed to run docker. Is Docker installed?")?;

    if !output.status.success() {
        anyhow::bail!(
            "Docker is not running. On Windows, auto-building requires Docker Desktop.\n\
             \n\
             Start Docker Desktop and try again, or add a Dockerfile to your project."
        );
    }

    Ok(())
}

fn parse_docker_summary(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.contains("Building") && line.contains("FINISHED") {
            if let Some(start) = line.find("Building ") {
                let rest = &line[start + 9..];
                if let Some(end) = rest.find(')') {
                    let duration = rest[..end].trim();
                    return Some(format!("Built in {}", duration));
                }
            }
        }
    }
    None
}

fn parse_railpack_summary(output: &str) -> Option<String> {
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.contains("Successfully") || trimmed.contains("built") {
            return Some("Built successfully".to_string());
        }
    }
    None
}

fn save_tar(image_tag: &str) -> Result<SavedImage> {
    let safe_name = image_tag.replace(['/', ':'], "-");
    let tar_path = std::env::temp_dir().join(format!("l8b-{}.tar", safe_name));
    let gz_path = std::env::temp_dir().join(format!("l8b-{}.tar.gz", safe_name));
    let tar_path_str = tar_path.to_string_lossy().to_string();

    // Use the tag as the image reference — docker save/load preserves tags,
    // and OCI format tars may have a different manifest digest than the local config digest.
    let image_id = image_tag.to_string();

    // docker save → uncompressed tar
    let output = Command::new("docker")
        .args(["save", "-o", &tar_path_str, image_tag])
        .output()
        .context("failed to run docker save")?;

    if !output.status.success() {
        anyhow::bail!(
            "docker save failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let image_size = std::fs::metadata(&tar_path)?.len();

    // Compress with gzip (using flate2 — cross-platform, no external tool needed)
    let tar_file = std::fs::File::open(&tar_path)?;
    let gz_file = std::fs::File::create(&gz_path)?;
    let mut encoder = flate2::write::GzEncoder::new(gz_file, flate2::Compression::default());
    std::io::copy(&mut std::io::BufReader::new(tar_file), &mut encoder)?;
    encoder.finish()?;

    let compressed_size = std::fs::metadata(&gz_path)?.len();

    // Remove uncompressed tar
    let _ = std::fs::remove_file(&tar_path);

    Ok(SavedImage {
        path: gz_path.to_string_lossy().to_string(),
        image_id,
        image_size,
        compressed_size,
    })
}

fn ensure_buildkit(quiet: bool, ci_mode: bool) -> Result<String> {
    const BUILDKIT_CONTAINER: &str = "buildkit";
    const BUILDKIT_HOST_DEFAULT: &str = "docker-container://buildkit";

    if let Ok(host) = std::env::var("BUILDKIT_HOST") {
        if !host.is_empty() {
            return Ok(host);
        }
    }

    let output = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name={}", BUILDKIT_CONTAINER),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .context("failed to check running containers. Is Docker running?")?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().contains(BUILDKIT_CONTAINER) {
            if !quiet && !ci_mode { println!("  {} BuildKit is running", "✔".green()); }
            return Ok(BUILDKIT_HOST_DEFAULT.to_string());
        }
    }

    if !quiet && !ci_mode { println!("  🧑 Starting BuildKit..."); }
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--privileged",
            "-d",
            "--name",
            BUILDKIT_CONTAINER,
            "moby/buildkit",
        ])
        .output()
        .context("failed to start BuildKit container. Is Docker running?")?;

    if !output.status.success() {
        anyhow::bail!("failed to start BuildKit. Make sure Docker is running and try again.");
    }

    if !quiet && !ci_mode {
        let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        println!("  {} BuildKit started — {}", "✔".green(), container_id.dimmed());
    }
    Ok(BUILDKIT_HOST_DEFAULT.to_string())
}
