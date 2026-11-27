use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::build;
use crate::ci::CiMode;
use crate::config;
use crate::out::Out;
use crate::project_config;
use crate::ship;

#[derive(Deserialize)]
pub(crate) struct PushDefault {
    pub host: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub insecure: bool,
}

impl PushDefault {
    pub fn credentials(&self) -> (Option<String>, Option<String>) {
        (self.username.clone(), self.password.clone())
    }

    /// Build a standard `<host>[/<namespace>]/<name>:<tag>` reference.
    /// Uses `<username>/` as namespace when present (required by docker.io/ghcr.io
    /// and standard on Zot/Harbor/GitLab), and picks git tag > git short SHA > latest.
    pub fn image_tag(&self, name: &str, project_dir: &Path) -> String {
        let host = self.host.trim_end_matches('/');
        let host = if host == "index.docker.io" { "docker.io" } else { host };
        let repo = match self.username.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(user) => {
                let ns: String = user
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
                    .collect();
                format!("{host}/{ns}/{name}")
            }
            None => format!("{host}/{name}"),
        };
        let tag = resolve_git_or_latest_tag(project_dir);
        format!("{repo}:{tag}")
    }
}

fn git_stdout(project_dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(project_dir).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if val.is_empty() { None } else { Some(val) }
}

fn utc_stamp() -> String {
    let secs =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil-from-days algorithm
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(mo <= 2);
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

fn resolve_git_or_latest_tag(project_dir: &Path) -> String {
    if let Some(git_tag) = git_stdout(project_dir, &["describe", "--tags", "--exact-match", "HEAD"]) {
        let clean: String = git_tag
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
            .collect();
        if !clean.is_empty() {
            return clean;
        }
    }
    git_stdout(project_dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(utc_stamp)
}

pub(crate) async fn fetch_push_default(client: &reqwest::Client, server: &str) -> Option<PushDefault> {
    let base = server.trim_end_matches('/');
    let res = client.get(format!("{base}/registries/push-default")).send().await.ok()?;
    if res.status() != reqwest::StatusCode::OK {
        return None;
    }
    res.json::<PushDefault>().await.ok()
}

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

pub(crate) fn push_image_with_latest(
    image_tag: &str,
    user: Option<&str>,
    pass: Option<&str>,
    insecure: bool,
) -> Result<()> {
    push_image(image_tag, user, pass, insecure)?;
    if let Some((repo, tag)) = split_repo_tag(image_tag)
        && tag != "latest"
    {
        let latest_tag = format!("{repo}:latest");
        let status =
            Command::new("docker").args(["tag", image_tag, &latest_tag]).status().context("failed to tag :latest")?;
        if !status.success() {
            bail!("docker tag {latest_tag} failed");
        }
        push_image(&latest_tag, user, pass, insecure)?;
    }
    Ok(())
}

fn split_repo_tag(image_ref: &str) -> Option<(&str, &str)> {
    let last_slash = image_ref.rfind('/');
    let last_colon = image_ref.rfind(':')?;
    if last_slash.is_some_and(|s| last_colon < s) {
        return None;
    }
    Some((&image_ref[..last_colon], &image_ref[last_colon + 1..]))
}

pub(crate) fn push_image(image_tag: &str, user: Option<&str>, pass: Option<&str>, insecure: bool) -> Result<()> {
    let has_skopeo = Command::new("skopeo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    let mut cmd = if has_skopeo {
        let mut c = Command::new("skopeo");
        c.args(["copy", "--format", "oci"]);
        c
    } else {
        let mut c = Command::new("docker");
        c.args([
            "run",
            "--rm",
            "--add-host",
            "host.docker.internal:host-gateway",
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            "quay.io/skopeo/stable",
            "copy",
            "--format",
            "oci",
        ]);
        c
    };

    if insecure {
        cmd.arg("--dest-tls-verify=false");
    }
    if let (Some(u), Some(p)) = (user, pass) {
        cmd.arg("--dest-creds").arg(format!("{u}:{p}"));
    }
    cmd.arg(format!("docker-daemon:{image_tag}"));
    cmd.arg(format!("docker://{image_tag}"));

    let status = cmd.status().context("failed to push image with skopeo")?;
    if !status.success() {
        bail!("skopeo copy failed with exit code {:?}", status.code());
    }
    Ok(())
}

pub(crate) async fn run(args: BuildArgs, ci_mode: &CiMode, out: &Out) -> Result<()> {
    let started = std::time::Instant::now();

    let server_push = if args.push {
        let cfg = config::CliConfig::load(None, None);
        if let Ok(target) = auth::resolve_target(&cfg, &args.path) {
            fetch_push_default(&target.client, &target.server).await
        } else {
            None
        }
    } else {
        None
    };

    let mut tag = args.tag.unwrap_or_else(|| default_image_tag(&args.path));
    if args.push
        && registry_host(&tag).is_none()
        && let Some(ref pd) = server_push
    {
        let name_only = tag.split(':').next().unwrap_or(&tag);
        tag = pd.image_tag(name_only, &args.path);
    }

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
        let (srv_user, srv_pass) = server_push.as_ref().map_or((None, None), PushDefault::credentials);
        let user = args.registry_user.or_else(|| std::env::var("L8B_REGISTRY_USER").ok()).or(srv_user);
        let pass = args.registry_pass.or_else(|| std::env::var("L8B_REGISTRY_PASS").ok()).or(srv_pass);
        let insecure = args.insecure || server_push.as_ref().is_some_and(|pd| pd.insecure);
        ci_mode.println(&format!("Pushing {tag}..."));
        push_image_with_latest(&tag, user.as_deref(), pass.as_deref(), insecure)?;
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

#[cfg(test)]
mod tests {
    use super::PushDefault;
    use std::path::Path;

    #[test]
    fn push_default_tag_formats_namespace_and_docker_io() {
        let tmp = std::env::temp_dir();
        let hub = PushDefault {
            host: "index.docker.io".into(),
            username: Some("Sandeep".into()),
            password: None,
            insecure: false,
        };
        let hub_tag = hub.image_tag("wallos", Path::new(&tmp));
        assert!(hub_tag.starts_with("docker.io/sandeep/wallos:"), "got {hub_tag}");

        let zot = PushDefault {
            host: "100.97.190.60:5010".into(),
            username: Some("zot-admin".into()),
            password: None,
            insecure: true,
        };
        let zot_tag = zot.image_tag("wallos", Path::new(&tmp));
        assert!(zot_tag.starts_with("100.97.190.60:5010/zot-admin/wallos:"), "got {zot_tag}");

        let anon = PushDefault { host: "localhost:5010".into(), username: None, password: None, insecure: true };
        let anon_tag = anon.image_tag("wallos", Path::new(&tmp));
        assert!(anon_tag.starts_with("localhost:5010/wallos:"), "got {anon_tag}");
    }
}
