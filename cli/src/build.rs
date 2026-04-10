use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;
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
        let output = Command::new("docker")
            .args([
                "run", "--rm",
                "-v", &format!("{}:/app", project_dir_str),
                "--entrypoint", "sh",
                crate::config::RAILPACK_IMAGE,
                "-c", "railpack info /app --format json 2>/dev/null",
            ])
            .env("MSYS_NO_PATHCONV", "1")
            .output();

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
    pub image_size: u64,
    pub compressed_size: u64,
}

/// Detect build strategy and produce a Docker image tar file.
/// When `ci_mode` is true, build logs are suppressed to prevent secret leakage.
pub async fn build_project(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    ci_mode: bool,
) -> Result<SavedImage> {
    build_project_inner(project_dir, dockerfile, image_tag, true, ci_mode).await
}

async fn build_project_inner(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    quiet: bool,
    ci_mode: bool,
) -> Result<SavedImage> {
    let has_dockerfile = if let Some(df) = dockerfile {
        project_dir.join(df).exists()
    } else {
        project_dir.join("Dockerfile").exists()
    };

    // Auto-generate .dockerignore from .gitignore if missing, so
    // railpack/BuildKit doesn't send node_modules etc. into the context.
    let _dockerignore_guard = ensure_dockerignore(project_dir);

    let result = if has_dockerfile {
        build_with_docker(project_dir, dockerfile, image_tag, quiet, ci_mode).await
    } else if cfg!(target_os = "windows") {
        check_docker_available()?;
        build_with_railpack_docker(project_dir, image_tag, quiet, ci_mode).await
    } else {
        build_with_railpack_native(project_dir, image_tag, quiet, ci_mode).await
    };

    // Temp .dockerignore is cleaned up when _dockerignore_guard drops
    result
}

async fn build_with_docker(
    project_dir: &Path,
    dockerfile: Option<&str>,
    image_tag: &str,
    quiet: bool,
    ci_mode: bool,
) -> Result<SavedImage> {
    if !quiet && !ci_mode {
        println!("Building with Docker...");
    }

    let mut cmd = Command::new("docker");
    cmd.arg("build");
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
    quiet: bool,
    ci_mode: bool,
) -> Result<SavedImage> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
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

        cmd.args([
            RAILPACK_IMAGE,
            "build", "--name", image_tag, "/app",
        ]);
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
    quiet: bool,
    ci_mode: bool,
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
        cmd.args(["build", "--name", image_tag, "."]);
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

/// If `.dockerignore` is missing, generate one from `.gitignore` so
/// railpack/BuildKit excludes files the user already ignores in git.
/// Returns a guard that removes the temp file on drop, or `None` if no file was created.
fn ensure_dockerignore(project_dir: &Path) -> Option<DockerignoreGuard> {
    let dockerignore = project_dir.join(".dockerignore");
    if dockerignore.exists() {
        return None;
    }

    let gitignore = project_dir.join(".gitignore");
    let content = if gitignore.exists() {
        // Convert .gitignore patterns to .dockerignore format:
        // strip leading "/" since .dockerignore uses relative paths
        std::fs::read_to_string(&gitignore).unwrap_or_default()
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('/') {
                    trimmed[1..].to_string()
                } else {
                    trimmed.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        // Sensible defaults when no .gitignore either
        "node_modules\n.git\n.next\ntarget\n".to_string()
    };

    if content.trim().is_empty() {
        return None;
    }

    if std::fs::write(&dockerignore, &content).is_ok() {
        Some(DockerignoreGuard(dockerignore))
    } else {
        None
    }
}

/// RAII guard that removes the temp `.dockerignore` on drop.
struct DockerignoreGuard(std::path::PathBuf);

impl Drop for DockerignoreGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// Shared helpers

/// Return directory names to mask in the Docker build context.
/// Reads `.dockerignore` if present, otherwise falls back to `.gitignore`.
/// Only returns simple directory names (no wildcards, no nested paths)
/// that actually exist on disk.
fn gitignored_dirs(project_dir: &Path) -> Vec<String> {
    let ignore_file = if project_dir.join(".dockerignore").exists() {
        project_dir.join(".dockerignore")
    } else {
        project_dir.join(".gitignore")
    };

    let content = match std::fs::read_to_string(&ignore_file) {
        Ok(c) => c,
        Err(_) => return vec![".git".to_string()],
    };

    let mut dirs: Vec<String> = vec![".git".to_string()];

    for line in content.lines() {
        let line = line.trim();
        // Skip comments, empty lines, and negation patterns
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        // Strip leading / and trailing /
        let pattern = line.trim_start_matches('/').trim_end_matches('/');
        // Only simple names — skip wildcards and nested paths
        if pattern.contains('*') || pattern.contains('/') {
            continue;
        }
        if project_dir.join(pattern).is_dir() {
            dirs.push(pattern.to_string());
        }
    }

    dirs
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
