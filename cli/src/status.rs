use std::collections::HashSet;

use anyhow::Result;
use colored::Colorize;
use litebin_common::types::ProjectStatus;
use serde::Serialize;

#[derive(Serialize)]
pub struct ServiceResult {
    pub service_name: String,
    pub status: String,
    pub is_public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
}

#[derive(Serialize)]
pub struct HealthResult {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct StatusResult {
    pub project_id: String,
    pub name: String,
    pub status: String,
    pub background: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthResult>,
}

/// Poll project status until it reaches a terminal state (running, stopped, error).
/// Returns the final status, or None on timeout (still deploying).
pub async fn poll_project_status(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    timeout_secs: u64,
    json: bool,
) -> Result<Option<ProjectStatus>> {
    let start = std::time::Instant::now();
    let poll_interval = std::time::Duration::from_secs(3);
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let mut seen_lines: HashSet<String> = HashSet::new();

    loop {
        let resp = client.get(format!("{}/projects/{}/stats", server.trim_end_matches('/'), project_id)).send().await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let json_body: serde_json::Value = r.json().await?;
                let status: ProjectStatus = json_body["status"]
                    .as_str()
                    .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
                    .unwrap_or(ProjectStatus::Stopped);
                if matches!(
                    status,
                    ProjectStatus::Running | ProjectStatus::Stopped | ProjectStatus::Error | ProjectStatus::Completed
                ) {
                    if !json {
                        fetch_and_print_new_logs(client, server, project_id, &mut seen_lines).await;
                    }
                    return Ok(Some(status));
                }
                if !json {
                    // Still deploying — show new deploy logs
                    fetch_and_print_new_logs(client, server, project_id, &mut seen_lines).await;
                }
            }
            _ => {
                // Non-success — ignore and retry
            }
        }

        if start.elapsed() >= timeout {
            return Ok(None);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Fetch deploy logs and print only lines not yet seen.
async fn fetch_and_print_new_logs(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    seen: &mut HashSet<String>,
) {
    let resp: Result<reqwest::Response, reqwest::Error> =
        client.get(format!("{}/projects/{}/deploy-logs", server.trim_end_matches('/'), project_id)).send().await;

    if let Ok(r) = resp
        && r.status().is_success()
        && let Ok(json_body) = r.json::<serde_json::Value>().await
        && let Some(lines) = json_body["lines"].as_array()
    {
        for line in lines {
            if let Some(text) = line.as_str()
                && seen.insert(text.to_string())
            {
                println!("    {}", text.dimmed());
            }
        }
    }
}

/// Build the typed status result (project + live service stats).
pub async fn build_status_result(client: &reqwest::Client, server: &str, project_id: &str) -> Result<StatusResult> {
    let resp = client.get(format!("{}/projects/{}", server.trim_end_matches('/'), project_id)).send().await?;

    if !resp.status().is_success() {
        return Err(crate::out::fail(
            format!("project '{}' not found (HTTP {})", project_id, resp.status()),
            "l8b status --project <id>, or list projects via the API: GET /projects".to_string(),
        ));
    }

    let project: serde_json::Value = resp.json().await?;
    let stats = client
        .get(format!("{}/projects/{}/stats", server.trim_end_matches('/'), project_id))
        .send()
        .await
        .ok()
        .filter(|response| response.status().is_success());
    let stats_json = match stats {
        Some(response) => response.json::<serde_json::Value>().await.ok(),
        None => None,
    };

    let status: String = project["status"].as_str().unwrap_or("unknown").to_string();
    let name = project["name"].as_str().unwrap_or(project_id).to_string();
    let background = project["is_background"].as_bool().unwrap_or(false);
    let services_raw = stats_json.as_ref().and_then(|stats| stats["services"].as_array());
    let image = project["public_stats"]["image"].as_str().map(str::to_string).or_else(|| {
        services_raw.and_then(|items| items.first()).and_then(|svc| svc["image"].as_str()).map(str::to_string)
    });
    let url = if background {
        None
    } else if let Some(d) = project["custom_domain"].as_str().filter(|s| !s.is_empty()) {
        Some(format!("https://{}", d))
    } else {
        let domain = crate::auth::fetch_platform_domain(client, server).await;
        Some(crate::auth::project_live_url(project_id, &domain))
    };

    let services = services_raw
        .map(|items| {
            items
                .iter()
                .map(|svc| ServiceResult {
                    service_name: svc["service_name"].as_str().unwrap_or("?").to_string(),
                    status: svc["status"].as_str().unwrap_or("?").to_string(),
                    is_public: svc["is_public"].as_bool().unwrap_or(false),
                    cpu_percent: svc["cpu_percent"].as_f64(),
                    memory_mb: svc["memory_mb"].as_u64(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(StatusResult {
        project_id: project_id.to_string(),
        name,
        status,
        background,
        url,
        image,
        services,
        health: None,
    })
}

/// Probe the public URL. Accepts self-signed certs: this checks reachability,
/// not certificate validity. A sleeping project is woken by the request.
pub async fn probe_url(url: &str) -> HealthResult {
    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return HealthResult {
                url: url.to_string(),
                http_status: None,
                latency_ms: None,
                ok: false,
                error: Some(e.to_string()),
            };
        }
    };

    let start = std::time::Instant::now();
    match client.get(url).send().await {
        Ok(resp) => HealthResult {
            url: url.to_string(),
            http_status: Some(resp.status().as_u16()),
            latency_ms: Some(start.elapsed().as_millis() as u64),
            ok: resp.status().is_success(),
            error: None,
        },
        Err(e) => HealthResult {
            url: url.to_string(),
            http_status: None,
            latency_ms: Some(start.elapsed().as_millis() as u64),
            ok: false,
            error: Some(e.to_string()),
        },
    }
}

/// Human rendering of a status result.
pub fn render_status_human(result: &StatusResult) {
    let status_colored = match result.status.as_str() {
        "running" => result.status.green().bold(),
        "stopped" => result.status.dimmed(),
        "deploying" | "importing" => result.status.yellow().bold(),
        "error" => result.status.red().bold(),
        _ => result.status.normal(),
    };

    println!();
    println!("  {} {}", "Project:".dimmed(), result.name.cyan());
    println!("  {} {}", "ID:".dimmed(), result.project_id.dimmed());
    println!("  {} {}", "Status:".dimmed(), status_colored);
    match &result.url {
        Some(url) => println!("  {} {}", "URL:".dimmed(), url.cyan()),
        None => println!("  {} {}", "URL:".dimmed(), "No managed URL (background project)".dimmed()),
    }
    if let Some(img) = &result.image {
        let short = if img.len() > 40 { &img[..37] } else { img };
        println!("  {} {}", "Image:".dimmed(), short.dimmed());
    }
    if result.services.len() > 1 {
        println!("  {} {}", "Services:".dimmed(), format!("{} services", result.services.len()).cyan());
    }

    if !result.services.is_empty() {
        println!();
        for svc in &result.services {
            let svc_status = match svc.status.as_str() {
                "running" => "running".green(),
                "stopped" => "stopped".dimmed(),
                s => s.yellow(),
            };
            let pub_tag = if svc.is_public && !result.background { " (public)".dimmed() } else { "".dimmed() };
            let stats = match (svc.cpu_percent, svc.memory_mb) {
                (Some(c), Some(m)) => format!("  {:.1}% cpu, {}MB mem", c, m),
                (Some(c), None) => format!("  {:.1}% cpu", c),
                (None, Some(m)) => format!("  {}MB mem", m),
                _ => String::new(),
            };
            println!("    {} {}{}{}", svc.service_name.cyan(), svc_status, pub_tag, stats.dimmed());
        }
    }

    if let Some(h) = &result.health {
        println!();
        match (h.http_status, h.ok) {
            (Some(code), true) => println!(
                "  {} {} in {}",
                "Healthy:".dimmed(),
                format!("HTTP {code}").green(),
                format!("{}ms", h.latency_ms.unwrap_or(0)).dimmed()
            ),
            (Some(code), false) => println!("  {} {}", "Healthy:".dimmed(), format!("HTTP {code} (non-2xx)").red()),
            (None, _) => println!(
                "  {} {}",
                "Healthy:".dimmed(),
                format!("unreachable: {}", h.error.clone().unwrap_or_default()).red()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal one-shot HTTP server for probe tests.
    fn spawn_responder(status_line: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(format!("{status_line}\r\nContent-Length: 0\r\n\r\n").as_bytes());
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn probe_url_reports_2xx_as_healthy() {
        let url = spawn_responder("HTTP/1.1 204 No Content");
        let result = probe_url(&url).await;
        assert!(result.ok);
        assert_eq!(result.http_status, Some(204));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn probe_url_reports_5xx_as_unhealthy() {
        let url = spawn_responder("HTTP/1.1 503 Service Unavailable");
        let result = probe_url(&url).await;
        assert!(!result.ok);
        assert_eq!(result.http_status, Some(503));
    }
}
