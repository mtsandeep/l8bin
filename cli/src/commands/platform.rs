//! `l8b platform domain` — manage the server's own platform domain (admin
//! scope). This is the platform-level domain (`{project}.{domain}`), not a
//! project custom domain (`l8b domain set`).

use anyhow::{Result, bail};
use serde::Serialize;

use crate::auth;
use crate::config;
use crate::out::{self, Out};

/// Map 403s (embedded in api_* error text) to an admin-pairing hint.
fn map_admin_err(e: anyhow::Error) -> anyhow::Error {
    let msg = format!("{e:#}");
    if msg.contains("(403") {
        out::fail(msg, "admin scope required: l8b login --server <url> --pair (pick admin when approving)")
    } else {
        e
    }
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v[key].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

#[derive(Serialize)]
struct PlatformDomainResult {
    domain: String,
    dashboard_url: String,
    routing_mode: Option<String>,
    tryout: Option<bool>,
}

/// `l8b platform domain show` — /settings when the credential is admin,
/// read-scoped /meta otherwise.
pub(crate) async fn show(server_flag: Option<&str>, token_flag: Option<&str>, out: &Out) -> Result<()> {
    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    let (client, server) = (target.client, target.server);

    let result = match auth::api_get(&client, &server, "/settings").await {
        Ok(v) => PlatformDomainResult {
            dashboard_url: format!(
                "https://{}.{}",
                v["dashboard_subdomain"].as_str().unwrap_or_default(),
                v["domain"].as_str().unwrap_or_default()
            ),
            domain: v["domain"].as_str().unwrap_or_default().to_string(),
            routing_mode: v["routing_mode"].as_str().map(str::to_string),
            tryout: v["tryout"].as_bool(),
        },
        Err(e) => {
            if !format!("{e:#}").contains("(403") {
                return Err(e);
            }
            let meta = auth::api_get(&client, &server, "/meta").await?;
            PlatformDomainResult {
                dashboard_url: format!(
                    "https://{}.{}",
                    meta["dashboard_subdomain"].as_str().unwrap_or_default(),
                    meta["domain"].as_str().unwrap_or_default()
                ),
                domain: meta["domain"].as_str().unwrap_or_default().to_string(),
                routing_mode: meta["routing_mode"].as_str().map(str::to_string),
                tryout: None,
            }
        }
    };

    out.ok(&result);
    if !out.json {
        println!("platform domain: {}", result.domain);
        println!("dashboard:        {}", result.dashboard_url);
        if let Some(mode) = &result.routing_mode {
            println!("routing mode:     {mode}");
        }
        if result.tryout == Some(true) {
            println!("tryout DNS (sslip.io / nip.io) — Let's Encrypt may fail; not for production");
        }
    }
    Ok(())
}

pub(crate) struct DomainSetArgs {
    pub domain: String,
    pub ack_dns: bool,
    pub timeout: Option<u64>,
}

#[derive(Serialize)]
struct DomainSetResult {
    domain: String,
    old_domain: Option<String>,
    status: String,
    dashboard_url: String,
}

/// `l8b platform domain set <domain>` — preflight → apply → poll to completion.
pub(crate) async fn domain_set(
    args: DomainSetArgs,
    server_flag: Option<&str>,
    token_flag: Option<&str>,
    ci_mode: &crate::ci::CiMode,
    out: &Out,
) -> Result<()> {
    let domain = args
        .domain
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('.')
        .trim_end_matches('/')
        .to_ascii_lowercase();

    let cfg = config::CliConfig::load(server_flag, token_flag);
    let target = auth::resolve_target(&cfg, std::path::Path::new("."))?;
    let (client, server) = (target.client, target.server);

    // Preflight
    let pre =
        auth::api_post_json(&client, &server, "/settings/domain/preflight", &serde_json::json!({ "domain": domain }))
            .await
            .map_err(map_admin_err)?;
    let errors = strings(&pre, "errors");
    if !errors.is_empty() {
        bail!(out::fail(
            format!("preflight failed: {}", errors.join("; ")),
            "fix the errors above (DNS / Cloudflare zone) and re-run"
        ));
    }
    let warnings = strings(&pre, "warnings");
    let acknowledge = if warnings.is_empty() || args.ack_dns {
        true
    } else if out.json || ci_mode.enabled {
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        bail!(out::fail(
            "preflight returned warnings; confirmation required",
            format!("review the warnings and re-run with --ack-dns: l8b platform domain set {domain} --ack-dns")
        ));
    } else {
        for w in &warnings {
            println!("warning: {w}");
        }
        dialoguer::Confirm::new()
            .with_prompt("Apply the domain change anyway?")
            .default(false)
            .interact()
            .unwrap_or(false)
    };
    if !acknowledge {
        out.note("Aborted.");
        return Ok(());
    }

    // Apply
    let apply = auth::api_post_json(
        &client,
        &server,
        "/settings/domain/apply",
        &serde_json::json!({ "domain": domain, "acknowledge_dns": true }),
    )
    .await
    .map_err(map_admin_err)?;
    let Some(job_id) = apply["job_id"].as_str().map(str::to_string) else {
        bail!("server did not return a job_id");
    };

    poll_job(&client, &server, &job_id, args.timeout.unwrap_or(300), out).await
}

/// Poll a domain job until terminal; on timeout exit 0 with the running status.
async fn poll_job(client: &reqwest::Client, server: &str, job_id: &str, timeout_secs: u64, out: &Out) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut printed: Vec<(usize, String)> = Vec::new();

    loop {
        let job = match auth::api_get(client, server, &format!("/settings/domain/jobs/{job_id}")).await {
            Ok(v) => v,
            // Transient failures — keep polling until the deadline.
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => return Err(e),
        };

        if !out.json
            && let Some(steps) = job["steps"].as_array()
        {
            for (idx, step) in steps.iter().enumerate() {
                let status = step["status"].as_str().unwrap_or_default().to_string();
                let key = (idx, status.clone());
                if !printed.contains(&key) && !status.is_empty() && status != "pending" && status != "running" {
                    printed.push(key);
                    let label = step["label"].as_str().unwrap_or_default();
                    match status.as_str() {
                        "done" => println!("  ✓ {label}"),
                        "skipped" => println!("  - {label} (skipped)"),
                        "failed" => println!("  ✗ {label}"),
                        _ => {}
                    }
                }
            }
        }

        match job["status"].as_str() {
            Some("completed") => {
                let result = DomainSetResult {
                    domain: job["domain"].as_str().unwrap_or_default().to_string(),
                    old_domain: job["old_domain"].as_str().map(str::to_string),
                    status: "completed".to_string(),
                    dashboard_url: job["dashboard_url"].as_str().unwrap_or_default().to_string(),
                };
                out.ok(&result);
                if !out.json {
                    println!("Platform domain is now {}.", result.domain);
                    println!("Sign in again at {}", result.dashboard_url);
                }
                return Ok(());
            }
            Some("failed") => {
                let detail = job["error"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        job["steps"]
                            .as_array()
                            .and_then(|steps| steps.iter().find_map(|s| s["error"].as_str().map(str::to_string)))
                    })
                    .unwrap_or_else(|| "unknown step failure".to_string());
                bail!(out::fail(
                    format!("domain change failed: {detail}"),
                    "resume from the dashboard (Settings → Change domain → Retry) or re-run l8b platform domain set"
                ));
            }
            status if std::time::Instant::now() >= deadline => {
                let result = DomainSetResult {
                    domain: job["domain"].as_str().unwrap_or_default().to_string(),
                    old_domain: job["old_domain"].as_str().map(str::to_string),
                    status: format!("{} (timed out waiting)", status.unwrap_or("running")),
                    dashboard_url: job["dashboard_url"].as_str().unwrap_or_default().to_string(),
                };
                out.ok(&result);
                if !out.json {
                    println!("Still running after {timeout_secs}s — check the dashboard Settings page.");
                }
                return Ok(());
            }
            _ => {}
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
