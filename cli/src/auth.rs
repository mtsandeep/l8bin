use anyhow::{Context, Result};
use litebin_common::types::NodeStatus;
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};

use crate::config::CliConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub server: String,
    pub cookie: String,
}

const SESSION_FILE: &str = "session.json";

pub fn session_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(crate::config::APP_DIR)
        .join(SESSION_FILE)
}

pub fn load_session() -> Option<Session> {
    let path = session_path();
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(session)?;
    std::fs::write(&path, content)?;
    Ok(())
}

pub fn clear_session() -> Result<()> {
    let path = session_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub async fn login(server: &str) -> Result<()> {
    let server = if server.starts_with("http://") || server.starts_with("https://") {
        server.to_string()
    } else {
        format!("https://{}", server)
    };
    println!("Server: {}", server);
    let username = dialoguer::Input::<String>::new()
        .with_prompt("Username")
        .interact_text()?;

    let password = rpassword::prompt_password("Password: ")?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/auth/login", server.trim_end_matches('/')))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await
        .context("login request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("login failed ({}): {}", status, body);
    }

    // Extract Set-Cookie header
    let cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");

    if cookie.is_empty() {
        anyhow::bail!("login succeeded but no session cookie received");
    }

    let session = Session {
        server: server.trim_end_matches('/').to_string(),
        cookie,
    };
    save_session(&session)?;
    crate::config::CliConfig::save(Some(&session.server), None)?;
    println!("Authenticated. Session saved.");
    Ok(())
}

/// Build a reqwest client with the appropriate auth headers.
/// Priority: deploy token > session cookie.
/// Returns an error if neither a token nor a session is available.
pub fn authenticated_client(config: &CliConfig) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();

    if let Some(token) = &config.token {
        let val = format!("Bearer {}", token);
        headers.insert(
            "Authorization",
            val.parse().map_err(|e| anyhow::anyhow!("invalid token: {}", e))?,
        );
    } else if let Some(session) = load_session() {
        headers.insert(
            "Cookie",
            HeaderValue::from_str(&session.cookie).map_err(|e| anyhow::anyhow!("invalid session cookie: {}", e))?,
        );
    } else {
        anyhow::bail!("not authenticated. Run: l8b login --server <url>  or  set L8B_TOKEN");
    }

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    Ok(client)
}

pub fn resolve_server(config: &CliConfig) -> Result<String> {
    if let Some(server) = &config.server {
        Ok(server.trim_end_matches('/').to_string())
    } else if let Some(session) = load_session() {
        Ok(session.server)
    } else {
        anyhow::bail!("no server URL. Use --server, L8B_SERVER env, or l8b login --server <url>")
    }
}

/// POST to the API using session (cookie) auth.
pub async fn session_post(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let session = load_session()
        .ok_or_else(|| anyhow::anyhow!("not logged in. Run: l8b login --server <url>"))?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .post(&url)
        .header("Cookie", &session.cookie)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text)
        .unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// GET from the API using session (cookie) auth.
pub async fn session_get(
    client: &reqwest::Client,
    server: &str,
    path: &str,
) -> Result<serde_json::Value> {
    let session = load_session()
        .ok_or_else(|| anyhow::anyhow!("not logged in. Run: l8b login --server <url>"))?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .get(&url)
        .header("Cookie", &session.cookie)
        .send()
        .await
        .with_context(|| format!("GET {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text)
        .unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// POST multipart form to the API using session (cookie) auth.
pub async fn session_post_multipart(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    form: reqwest::multipart::Form,
) -> Result<serde_json::Value> {
    let session = load_session()
        .ok_or_else(|| anyhow::anyhow!("not logged in. Run: l8b login --server <url>"))?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .post(&url)
        .header("Cookie", &session.cookie)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text)
        .unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// DELETE from the API using session (cookie) auth.
pub async fn session_delete(
    client: &reqwest::Client,
    server: &str,
    path: &str,
) -> Result<serde_json::Value> {
    let session = load_session()
        .ok_or_else(|| anyhow::anyhow!("not logged in. Run: l8b login --server <url>"))?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .delete(&url)
        .header("Cookie", &session.cookie)
        .send()
        .await
        .with_context(|| format!("DELETE {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text)
        .unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// Fetch the Platform Domain from GET /settings (same value the dashboard shows).
/// Falls back to deriving from the server URL if settings are unavailable (e.g. CI token auth).
pub async fn fetch_platform_domain(client: &reqwest::Client, server: &str) -> String {
    if let Ok(settings) = session_get(client, server, "/settings").await {
        if let Some(domain) = settings["domain"].as_str() {
            let domain = domain.trim();
            if !domain.is_empty() {
                return domain.to_string();
            }
        }
    }
    derive_domain_from_server(server)
}

/// Last-resort domain derivation when /settings is unavailable.
/// `https://dash.11b.in` → `11b.in`
fn derive_domain_from_server(server: &str) -> String {
    let host = server
        .trim()
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split('/').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 3 {
        parts[1..].join(".")
    } else {
        host.to_string()
    }
}

/// Public project URL using the Platform Domain: `https://{project_id}.{domain}`
pub fn project_live_url(project_id: &str, domain: &str) -> String {
    format!("https://{}.{}", project_id, domain.trim().trim_start_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::{derive_domain_from_server, project_live_url};

    #[test]
    fn strips_dashboard_subdomain() {
        assert_eq!(derive_domain_from_server("https://dash.11b.in"), "11b.in");
        assert_eq!(derive_domain_from_server("https://dash.11b.in/"), "11b.in");
    }

    #[test]
    fn keeps_apex_domain() {
        assert_eq!(derive_domain_from_server("https://example.com"), "example.com");
    }

    #[test]
    fn project_url_uses_platform_domain() {
        assert_eq!(project_live_url("board", "11b.in"), "https://board.11b.in");
        assert_ne!(project_live_url("board", "11b.in"), "board.https://dash.11b.in");
    }
}

#[derive(Debug, Deserialize)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub status: NodeStatus,
    pub architecture: Option<String>,
    pub recommended: Option<bool>,
}

/// Fetch online nodes from the server. Returns empty vec on failure.
pub async fn fetch_online_nodes(
    client: &reqwest::Client,
    server: &str,
) -> Vec<NodeInfo> {
    match session_get(client, server, "/nodes").await {
        Ok(resp) => {
            let nodes: Vec<NodeInfo> = serde_json::from_value(resp).unwrap_or_default();
            nodes.into_iter().filter(|n| n.status == NodeStatus::Online).collect()
        }
        Err(_) => Vec::new(),
    }
}
