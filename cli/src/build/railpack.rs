use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use super::SavedImage;
use super::context::gitignored_dirs;
use super::dockerbuild::{create_build_spinner, ensure_buildkit, save_tar};

type EnvPairs<'a> = &'a BTreeMap<String, String>;

// Railpack via Docker (Windows)

const RAILPACK_IMAGE: &str = crate::config::RAILPACK_IMAGE;

fn ensure_railpack_image(railpack_tag: &str, mise_version: &str, quiet: bool, ci_mode: bool) -> Result<()> {
    let inspect = Command::new("docker")
        .args(["image", "inspect", "--format", "{{index .Config.Labels \"version\"}}", RAILPACK_IMAGE])
        .output()?;
    let current_label = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
    let expected_label = format!("rp={} mise={}", railpack_tag, mise_version);

    if inspect.status.success() && current_label == expected_label {
        if !quiet && !ci_mode {
            println!("  {} Railpack image ready", "✔".green());
        }
        return Ok(());
    }

    // Remove stale image
    let _ = Command::new("docker").args(["rmi", "-f", RAILPACK_IMAGE]).output();

    if !quiet && !ci_mode {
        println!("  🔨 Building Railpack image...");
    }

    let tmp = std::env::temp_dir().join("l8b-railpack-image");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let label = format!("rp={railpack_tag} mise={mise_version}");
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
        label = label,
        rp_base = crate::config::RAILPACK_RELEASE_BASE,
        mise_base = crate::config::MISE_RELEASE_BASE,
    );

    std::fs::write(tmp.join("Dockerfile"), &dockerfile)?;

    let mut cmd = Command::new("docker");
    cmd.args(["build", "-t", RAILPACK_IMAGE, "."]).current_dir(&tmp);

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

    if !quiet && !ci_mode {
        println!("  {} Railpack image built", "✔".green());
    }
    Ok(())
}

pub(super) async fn build_with_railpack_docker(
    project_dir: &Path,
    image_tag: &str,
    env_pairs: EnvPairs<'_>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;

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

    let spinner = if ci_mode {
        None
    } else if quiet {
        Some(create_build_spinner())
    } else {
        None
    };

    let max_retries = crate::config::MAX_RETRIES;
    let mut last_output = None;

    for attempt in 1..=max_retries {
        let mut cmd = railpack_docker_command(project_dir, image_tag, platform, env_pairs, attempt == max_retries);

        if quiet || ci_mode {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            let output = cmd.output().context("failed to run Railpack in Docker. Is Docker Desktop running?")?;

            if output.status.success() {
                let result = save_tar(image_tag)?;
                if let Some(s) = &spinner {
                    s.finish_and_clear();
                }
                return Ok(result);
            }

            last_output = Some((
                String::from_utf8_lossy(&output.stderr).to_string(),
                String::from_utf8_lossy(&output.stdout).to_string(),
            ));
        } else {
            let status = cmd.status().context("failed to run Railpack in Docker. Is Docker Desktop running?")?;

            if status.success() {
                return save_tar(image_tag);
            }
        }

        if attempt < max_retries && !ci_mode {
            println!("  {} Build failed (attempt {}/{}), retrying...", "!".yellow(), attempt, max_retries);
        }
    }

    if let Some(s) = &spinner {
        s.finish_and_clear();
    }
    if let Some((stderr, stdout)) = last_output
        && !ci_mode
    {
        eprintln!("{}", stdout);
        eprintln!("{}", stderr);
    }
    anyhow::bail!("railpack build failed after {} attempts", max_retries);
}

// Railpack native (Linux / macOS)

pub(super) async fn build_with_railpack_native(
    project_dir: &Path,
    image_tag: &str,
    env_pairs: EnvPairs<'_>,
    quiet: bool,
    ci_mode: bool,
    platform: Option<&str>,
) -> Result<SavedImage> {
    let (railpack_bin, railpack_tag) = crate::railpack::ensure_railpack(ci_mode).await?;
    crate::mise::ensure_mise_for_railpack(&railpack_tag, ci_mode).await?;

    let buildkit_host = ensure_buildkit(quiet, ci_mode)?;

    let spinner = if ci_mode {
        None
    } else if quiet {
        Some(create_build_spinner())
    } else {
        None
    };

    if !quiet && !ci_mode {
        println!("No Dockerfile found. Building with Railpack...");
    }

    let max_retries = crate::config::MAX_RETRIES;
    let mut last_output = None;

    for attempt in 1..=max_retries {
        let mut cmd = railpack_native_command(
            &railpack_bin,
            project_dir,
            image_tag,
            platform,
            &buildkit_host,
            env_pairs,
            attempt == max_retries,
        );

        if quiet || ci_mode {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

            let output = cmd.output().context("failed to run railpack build")?;

            if output.status.success() {
                let result = save_tar(image_tag)?;
                if let Some(s) = &spinner {
                    s.finish_and_clear();
                }
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
            let status = cmd.status().context("failed to run railpack build")?;

            if status.success() {
                return save_tar(image_tag);
            }
        }

        if attempt < max_retries && !ci_mode {
            println!("  {} Build failed (attempt {}/{}), retrying...", "!".yellow(), attempt, max_retries);
        }
    }

    if let Some(s) = &spinner {
        s.finish_and_clear();
    }
    if let Some((stderr, stdout)) = last_output
        && !ci_mode
    {
        eprintln!("{}", stdout);
        eprintln!("{}", stderr);
    }
    anyhow::bail!("railpack build failed after {} attempts", max_retries)
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

/// `railpack build` inside the Railpack container. Env vars ride along as bare
/// `-e KEY` (docker pulls each value from this process's environment) plus
/// name-only `--env KEY` flags, keeping values off the command line.
fn railpack_docker_command(
    project_dir: &Path,
    image_tag: &str,
    platform: Option<&str>,
    env_pairs: EnvPairs<'_>,
    verbose: bool,
) -> Command {
    let mut cmd = Command::new("docker");
    cmd.args([
        "run",
        "--rm",
        "-v",
        &format!("{}:/app", project_dir.to_string_lossy()),
        "-v",
        "/var/run/docker.sock:/var/run/docker.sock",
        "-e",
        "BUILDKIT_HOST=docker-container://buildkit",
    ]);

    // Mask gitignored directories so the bind mount doesn't leak
    // node_modules, .git, etc. into the build context.
    for dir in gitignored_dirs(project_dir) {
        cmd.args(["--tmpfs", &format!("/app/{}", dir)]);
    }

    for key in env_pairs.keys() {
        cmd.args(["-e", key]);
    }

    cmd.arg(RAILPACK_IMAGE);
    cmd.args(["build", "--name", image_tag]);
    if let Some(p) = platform {
        cmd.args(["--platform", p]);
    }
    for key in env_pairs.keys() {
        cmd.args(["--env", key]);
    }
    cmd.arg("/app");
    if verbose {
        cmd.arg("--verbose");
    }

    cmd.env("MSYS_NO_PATHCONV", "1");
    for (key, value) in env_pairs {
        cmd.env(key, value);
    }
    cmd
}

/// Native `railpack build` with env vars on the process environment plus
/// name-only `--env KEY` registration (railpack reads values from its env).
fn railpack_native_command(
    railpack_bin: &Path,
    project_dir: &Path,
    image_tag: &str,
    platform: Option<&str>,
    buildkit_host: &str,
    env_pairs: EnvPairs<'_>,
    verbose: bool,
) -> Command {
    let mut cmd = Command::new(railpack_bin);
    cmd.args(["build", "--name", image_tag]);
    if let Some(p) = platform {
        cmd.args(["--platform", p]);
    }
    for key in env_pairs.keys() {
        cmd.args(["--env", key]);
    }
    cmd.arg(".");
    if verbose {
        cmd.arg("--verbose");
    }

    cmd.env("BUILDKIT_HOST", buildkit_host);
    for (key, value) in env_pairs {
        cmd.env(key, value);
    }
    cmd.current_dir(project_dir);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs() -> BTreeMap<String, String> {
        [("ALPHA".to_string(), "1".to_string()), ("BETA".to_string(), "hush".to_string())].into()
    }

    fn args_as_strings(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn native_cmd_registers_env_names_only() {
        let dir = std::env::temp_dir();
        let cmd = railpack_native_command(
            Path::new("railpack"),
            &dir,
            "l8b/app:latest",
            None,
            "docker-container://buildkit",
            &pairs(),
            false,
        );

        let args = args_as_strings(&cmd);
        let dot = args.iter().position(|a| a == ".").unwrap();
        let first_env = args.iter().position(|a| a == "--env").unwrap();
        assert!(first_env < dot, "--env flags must precede the positional: {args:?}");
        assert!(args.contains(&"ALPHA".to_string()) && args.contains(&"BETA".to_string()));
        assert!(!args.iter().any(|a| a == "1" || a == "hush"), "values must not appear in args: {args:?}");

        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().to_string(), v?.to_string_lossy().to_string())))
            .collect();
        assert!(envs.contains(&("ALPHA".to_string(), "1".to_string())));
        assert!(envs.contains(&("BETA".to_string(), "hush".to_string())));
        assert!(envs.contains(&("BUILDKIT_HOST".to_string(), "docker-container://buildkit".to_string())));
    }

    #[test]
    fn docker_cmd_bare_e_and_rp_env() {
        let dir = std::env::temp_dir();
        let cmd = railpack_docker_command(&dir, "l8b/app:latest", Some("linux/arm64"), &pairs(), true);

        let args = args_as_strings(&cmd);
        let image_pos = args.iter().position(|a| a == RAILPACK_IMAGE).unwrap();
        let bare_e = args.iter().position(|a| a == "-e").unwrap();
        assert!(bare_e < image_pos, "bare -e flags must precede the railpack image: {args:?}");
        assert!(args.contains(&"ALPHA".to_string()) && args.contains(&"BETA".to_string()));
        assert!(!args.iter().any(|a| a == "1" || a == "hush"), "values must not appear in args: {args:?}");

        let app_pos = args.iter().position(|a| a == "/app").unwrap();
        let rp_env = args[image_pos..].iter().position(|a| a == "--env").map(|i| i + image_pos).unwrap();
        assert!(rp_env < app_pos, "--env flags must precede /app: {args:?}");
        assert!(args.contains(&"--platform".to_string()) && args.contains(&"linux/arm64".to_string()));
        assert!(args.last().unwrap() == "--verbose");

        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().to_string(), v?.to_string_lossy().to_string())))
            .collect();
        assert!(envs.contains(&("ALPHA".to_string(), "1".to_string())));
        assert!(envs.contains(&("MSYS_NO_PATHCONV".to_string(), "1".to_string())));
    }
}
