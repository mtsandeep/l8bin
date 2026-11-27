use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::build;
use crate::ci::CiMode;
use crate::out::Out;
use crate::project_config;
use crate::ship;

#[derive(Serialize)]
struct BuildOutcome {
    tag: String,
    image_id: String,
    image_size: u64,
    pushed: bool,
    duration_secs: u64,
}

pub(crate) struct BuildArgs {
    pub tag: Option<String>,
    pub path: PathBuf,
    pub dockerfile: Option<String>,
    pub platform: Option<String>,
    pub push: bool,
    pub insecure: bool,
    pub registry_user: Option<String>,
    pub registry_pass: Option<String>,
    pub secret: Vec<PathBuf>,
    pub no_env: bool,
    pub keep_tar: Option<PathBuf>,
}

fn default_image_tag(path: &Path) -> String {
    let base = project_config::load(path)
        .and_then(|c| c.project)
        .or_else(|| {
            std::fs::canonicalize(path).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        })
        .unwrap_or_else(|| "app".to_string());
    let slug: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect();
    let name = slug.trim_matches('-');
    let name = if name.is_empty() { "app" } else { name };
    if let Ok(reg) = std::env::var("L8B_REGISTRY") {
        let reg = reg.trim().trim_end_matches('/');
        if !reg.is_empty() {
            return format!("{reg}/{name}:latest");
        }
    }
    format!("{name}:latest")
}

fn registry_host(image_tag: &str) -> Option<&str> {
    let first = image_tag.split('/').next()?;
    if first.contains('.') || first.contains(':') || first == "localhost" { Some(first) } else { None }
}

fn push_image(image_tag: &str, user: Option<&str>, pass: Option<&str>, insecure: bool) -> Result<()> {
    let has_skopeo = Command::new("skopeo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if has_skopeo {
        let mut cmd = Command::new("skopeo");
        cmd.args(["copy", "--format", "oci"]);
        if insecure {
            cmd.arg("--dest-tls-verify=false");
        }
        if let (Some(u), Some(p)) = (user, pass) {
            cmd.arg("--dest-creds").arg(format!("{u}:{p}"));
        }
        cmd.arg(format!("docker-daemon:{image_tag}"));
        cmd.arg(format!("docker://{image_tag}"));

        let status = cmd.status().context("failed to execute skopeo copy")?;
        if !status.success() {
            bail!("skopeo copy failed with exit code {:?}", status.code());
        }
        return Ok(());
    }

    if let (Some(u), Some(p), Some(host)) = (user, pass, registry_host(image_tag)) {
        let mut login = Command::new("docker")
            .args(["login", host, "-u", u, "--password-stdin"])
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to run docker login")?;
        if let Some(mut stdin) = login.stdin.take() {
            use std::io::Write;
            stdin.write_all(p.as_bytes())?;
        }
        let login_status = login.wait()?;
        if !login_status.success() {
            bail!("docker login to {host} failed");
        }
    }

    let status = Command::new("docker").args(["push", image_tag]).status().context("failed to run docker push")?;
    if !status.success() {
        bail!("docker push {image_tag} failed with exit code {:?}", status.code());
    }
    Ok(())
}

pub(crate) async fn run(args: BuildArgs, ci_mode: &CiMode, out: &Out) -> Result<()> {
    let started = std::time::Instant::now();
    let tag = args.tag.unwrap_or_else(|| default_image_tag(&args.path));
    let env_source = ship::EnvSource::from_flags(args.secret, args.no_env);
    let env_files = ship::resolve_env_files(&args.path, &env_source, false, ci_mode.enabled)?;
    let build_env = build::BuildEnv { context_files: env_files, external_files: Vec::new() };

    ci_mode.println(&format!("Building {tag}..."));
    let image = build::build_project(
        &args.path,
        args.dockerfile.as_deref(),
        &tag,
        build_env,
        ci_mode.enabled,
        args.platform.as_deref(),
    )
    .await?;

    let tar_path = PathBuf::from(&image.path);
    if args.push {
        let user = args.registry_user.or_else(|| std::env::var("L8B_REGISTRY_USER").ok());
        let pass = args.registry_pass.or_else(|| std::env::var("L8B_REGISTRY_PASS").ok());
        ci_mode.println(&format!("Pushing {tag}..."));
        push_image(&tag, user.as_deref(), pass.as_deref(), args.insecure)?;
    }

    if let Some(dest) = args.keep_tar {
        std::fs::copy(&tar_path, &dest).with_context(|| format!("failed to copy tar to {}", dest.display()))?;
    }
    let _ = std::fs::remove_file(&tar_path);

    let duration_secs = started.elapsed().as_secs();
    out.ok(&BuildOutcome {
        tag: tag.clone(),
        image_id: image.image_id,
        image_size: image.image_size,
        pushed: args.push,
        duration_secs,
    });
    if args.push {
        out.note(&format!("Built and pushed {tag} in {duration_secs}s."));
    } else {
        out.note(&format!("Built {tag} in {duration_secs}s."));
    }
    Ok(())
}
