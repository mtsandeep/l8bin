//! `l8b doctor` — environment sanity checks with recovery hints.

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::auth;
use crate::config;
use crate::out::Out;

#[derive(Serialize)]
struct CheckResult {
    name: String,
    ok: bool,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

pub(crate) async fn run(server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let mut checks: Vec<CheckResult> = Vec::new();

    // Target resolution: the refusal itself is the diagnosis.
    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = match auth::resolve_target(&cfg, std::path::Path::new(".")) {
        Ok(t) => t,
        Err(e) => {
            checks.push(CheckResult {
                name: "server".into(),
                ok: false,
                detail: crate::out::split_hint(&e).0,
                hint: crate::out::split_hint(&e).1,
            });
            finish(&checks, out);
            return Ok(());
        }
    };
    let server = target.server;

    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
    let health = client.get(format!("{server}/health")).send().await;
    match health {
        Ok(r) if r.status().is_success() => {
            // /health carries no version; /meta does (read-scoped, best-effort).
            let version = match auth::api_get(&target.client, &server, "/meta").await {
                Ok(m) => m["version"].as_str().map(str::to_string),
                Err(_) => None,
            };
            checks.push(CheckResult {
                name: "server".into(),
                ok: true,
                detail: match version {
                    Some(v) => format!("reachable (v{v})"),
                    None => "reachable".to_string(),
                },
                hint: None,
            });
        }
        Ok(r) => checks.push(CheckResult {
            name: "server".into(),
            ok: false,
            detail: format!("unhealthy (HTTP {})", r.status()),
            hint: Some("check the orchestrator container/service on the server".into()),
        }),
        Err(e) => checks.push(CheckResult {
            name: "server".into(),
            ok: false,
            detail: format!("unreachable: {e}"),
            hint: Some("verify the URL and that the server is running".into()),
        }),
    }

    // Auth (token or session) — /whoami identifies the credential
    match auth::api_get(&target.client, &server, "/whoami").await {
        Ok(me) => {
            let identity = if me["kind"].as_str() == Some("token") {
                let name = me["name"].as_str().unwrap_or("unnamed");
                let scope = me["scope"].as_str().unwrap_or("?");
                match me["project_id"].as_str() {
                    Some(p) => format!("token '{name}' (scope {scope}, project '{p}')"),
                    None => format!("token '{name}' (scope {scope})"),
                }
            } else {
                format!("session '{}'", me["username"].as_str().unwrap_or("?"))
            };
            checks.push(CheckResult {
                name: "auth".into(),
                ok: true,
                detail: format!("authenticated ({identity})"),
                hint: None,
            })
        }
        Err(e) => checks.push(CheckResult {
            name: "auth".into(),
            ok: false,
            detail: format!("credentials rejected: {e}"),
            hint: Some(auth::login_hint_env(Some(&server))),
        }),
    }

    // Local Docker (needed for builds/uploads)
    let docker_ok =
        std::process::Command::new("docker").arg("version").arg("--format").arg("{{.Server.Version}}").output();
    match docker_ok {
        Ok(o) if o.status.success() => checks.push(CheckResult {
            name: "docker".into(),
            ok: true,
            detail: format!("docker {}", String::from_utf8_lossy(&o.stdout).trim()),
            hint: None,
        }),
        _ => checks.push(CheckResult {
            name: "docker".into(),
            ok: false,
            detail: "docker daemon not reachable".into(),
            hint: Some("start Docker Desktop / the docker daemon (needed for l8b deploy builds)".into()),
        }),
    }

    // Project config — a workspace without l8b.toml isn't bound to anything.
    match crate::project_config::load(std::path::Path::new(".")) {
        Some(c) => checks.push(CheckResult {
            name: "l8b.toml".into(),
            ok: true,
            detail: format!("project {}", c.project.as_deref().unwrap_or("(project unset)")),
            hint: None,
        }),
        None => checks.push(CheckResult {
            name: "l8b.toml".into(),
            ok: false,
            detail: "not found in the current directory".into(),
            hint: Some("l8b init — records project defaults for agents and scripts".into()),
        }),
    }

    finish(&checks, out);
    Ok(())
}

fn finish(checks: &[CheckResult], out: &Out) {
    let all_ok = checks.iter().all(|c| c.ok);
    out.ok(&serde_json::json!({ "checks": checks, "healthy": all_ok }));

    if !out.json {
        println!();
        for c in checks {
            let mark = if c.ok { "✓".green() } else { "✗".red() };
            println!("  {mark} {}: {}", c.name.cyan(), c.detail);
            if let Some(h) = &c.hint {
                println!("    {} {h}", "→".dimmed());
            }
        }
        println!();
        if all_ok {
            println!("  {}", "All checks passed.".green());
        } else {
            println!("  {}", "Some checks failed — see hints above.".red());
        }
        println!();
    }

    if !all_ok {
        std::process::exit(1);
    }
}
