use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;
use std::process::{Command, Stdio};

use super::SavedImage;
use super::context::BuildContextGuard;

pub(super) async fn build_with_docker(
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

    let spinner = if ci_mode {
        None
    } else if quiet {
        Some(create_build_spinner())
    } else {
        None
    };

    if quiet || ci_mode {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().context("failed to run docker build. Is Docker installed?")?;

        if !output.status.success() {
            if let Some(s) = &spinner {
                s.finish_and_clear();
            }
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
        let status = cmd.status().context("failed to run docker build. Is Docker installed?")?;
        if !status.success() {
            anyhow::bail!("docker build failed with exit code {:?}", status.code());
        }
    }

    let result = save_tar(image_tag)?;
    if let Some(s) = &spinner {
        s.finish_and_clear();
    }
    Ok(result)
}

pub(super) fn create_build_spinner() -> indicatif::ProgressBar {
    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(indicatif::ProgressStyle::default_spinner().template("  🔨 {spinner} {msg}").unwrap());
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message("Building image...");
    spinner
}

pub(super) fn check_docker_available() -> Result<()> {
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
        if line.contains("Building")
            && line.contains("FINISHED")
            && let Some(start) = line.find("Building ")
        {
            let rest = &line[start + 9..];
            if let Some(end) = rest.find(')') {
                let duration = rest[..end].trim();
                return Some(format!("Built in {}", duration));
            }
        }
    }
    None
}

pub(super) fn save_tar(image_tag: &str) -> Result<SavedImage> {
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
        anyhow::bail!("docker save failed: {}", String::from_utf8_lossy(&output.stderr));
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

    Ok(SavedImage { path: gz_path.to_string_lossy().to_string(), image_id, image_size, compressed_size })
}

pub(super) fn ensure_buildkit(quiet: bool, ci_mode: bool) -> Result<String> {
    const BUILDKIT_CONTAINER: &str = "buildkit";
    const BUILDKIT_HOST_DEFAULT: &str = "docker-container://buildkit";

    if let Ok(host) = std::env::var("BUILDKIT_HOST")
        && !host.is_empty()
    {
        return Ok(host);
    }

    let output = Command::new("docker")
        .args(["ps", "--filter", &format!("name={}", BUILDKIT_CONTAINER), "--format", "{{.Names}}"])
        .output()
        .context("failed to check running containers. Is Docker running?")?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().contains(BUILDKIT_CONTAINER) {
            if !quiet && !ci_mode {
                println!("  {} BuildKit is running", "✔".green());
            }
            return Ok(BUILDKIT_HOST_DEFAULT.to_string());
        }
    }

    if !quiet && !ci_mode {
        println!("  🧑 Starting BuildKit...");
    }
    let output = Command::new("docker")
        .args(["run", "--rm", "--privileged", "-d", "--name", BUILDKIT_CONTAINER, "moby/buildkit"])
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
