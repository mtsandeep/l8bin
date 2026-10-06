use anyhow::{Context, Result};
use colored::Colorize;
use litebin_common::types::NodeStatus;
use reqwest::header::HeaderValue;
use serde::Deserialize;

use crate::config::{CliConfig, CredentialStore, normalize_server};

/// True when running as a subprocess of `l8b mcp` — recovery hints should
/// address the driving agent, not a human at a terminal.
pub fn serving_agent() -> bool {
    std::env::var("L8B_MCP").is_ok_and(|v| v == "1")
}

/// Recovery hint for "not authenticated", phrased per audience. Pure.
pub fn login_hint(server: Option<&str>, agent: bool) -> String {
    match (server, agent) {
        (Some(s), false) => format!("l8b login --server {s} --pair"),
        (Some(s), true) => format!("call the `setup` MCP tool with {{\"server\": \"{s}\"}}"),
        (None, false) => "l8b login --server <url> --pair".to_string(),
        (None, true) => "call the `setup` MCP tool (no arguments) to see the current state".to_string(),
    }
}

/// `login_hint` reading L8B_MCP.
pub fn login_hint_env(server: Option<&str>) -> String {
    login_hint(server, serving_agent())
}

/// A resolved deployment target: the server this command will hit, plus a
/// client authenticated with that server's credential.
pub struct Target {
    pub server: String,
    pub client: reqwest::Client,
}

pub fn client_for(auth: String) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if !auth.is_empty() {
        let (name, value) = if auth.starts_with("Bearer ") { ("Authorization", auth) } else { ("Cookie", auth) };
        headers.insert(
            name,
            HeaderValue::from_str(&value).map_err(|e| anyhow::anyhow!("invalid stored credential: {}", e))?,
        );
    }
    Ok(reqwest::Client::builder().default_headers(headers).timeout(std::time::Duration::from_secs(300)).build()?)
}

/// Resolve which server this command targets and build a client with that
/// server's credential. Resolution order: `--server`/L8B_SERVER > `l8b.toml`
/// `server` > the single stored login. Ambiguity is a refusal that lists the
/// choices — never a silent default.
pub fn resolve_target(cfg: &CliConfig, dir: &std::path::Path) -> Result<Target> {
    let store = CredentialStore::load();
    let toml_server = crate::project_config::load(dir).and_then(|c| c.server);
    let (server, auth) = pick_server(cfg.server.as_deref(), toml_server.as_deref(), cfg.token.as_deref(), &store)?;
    Ok(Target { server, client: client_for(auth)? })
}

/// The resolution rules, pure so they can be tested. Returns the server URL
/// and its auth header value (raw token overrides the stored credential).
pub fn pick_server(
    flag_server: Option<&str>,
    toml_server: Option<&str>,
    raw_token: Option<&str>,
    store: &CredentialStore,
) -> Result<(String, String)> {
    let bearer = |t: &str| format!("Bearer {t}");

    for (candidate, login_msg) in [
        (flag_server, "not logged in to {server}"),
        (toml_server, "this repo deploys to {server}, but you are not logged in to it"),
    ] {
        if let Some(c) = candidate {
            let server = normalize_server(c);
            match store.get(&server) {
                Some(cred) => {
                    let auth = raw_token.map(bearer).or_else(|| cred.auth_header()).unwrap_or_default();
                    return Ok((server, auth));
                }
                None if raw_token.is_some() => return Ok((server, bearer(raw_token.unwrap()))),
                None => {
                    return Err(crate::out::fail(
                        login_msg.replace("{server}", &server),
                        login_hint_env(Some(&server)),
                    ));
                }
            }
        }
    }

    let logged_in = store.logged_in_servers();
    match logged_in.len() {
        0 => Err(crate::out::fail("not logged in to any server", login_hint_env(None))),
        1 => {
            let server = logged_in[0].clone();
            let auth =
                raw_token.map(bearer).or_else(|| store.get(&server).and_then(|c| c.auth_header())).unwrap_or_default();
            Ok((server, auth))
        }
        _ => {
            let list = logged_in.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            let hint = if serving_agent() {
                "call the `setup` MCP tool (no arguments) — it reports the stored logins and binds this workspace to one"
            } else {
                "pass --server, set `server` in l8b.toml, or run l8b init"
            };
            Err(crate::out::fail(format!("ambiguous server — logged into: {list}"), hint))
        }
    }
}

/// Session cookie for a server, for endpoints that need a dashboard login
/// (project creation, token minting).
fn cookie_for(server: &str) -> Result<String> {
    CredentialStore::load().get(server).and_then(|c| c.cookie.clone()).ok_or_else(|| {
        crate::out::fail(
            format!("this command needs a dashboard login (username/password) to {server}"),
            format!("l8b login --server {server} --password"),
        )
    })
}

/// Username/password login (session-based, same access as the dashboard).
pub async fn login_password(server: &str) -> Result<()> {
    let server = normalize_server(server);
    println!("Server: {server}");

    let username: String = dialoguer::Input::new().with_prompt("Username").interact_text()?;
    let password = dialoguer::Password::new().with_prompt("Password").interact()?;

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

    let cookie =
        resp.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join("; ");
    if cookie.is_empty() {
        anyhow::bail!("login succeeded but no session cookie received");
    }

    let mut store = CredentialStore::load();
    store.upsert(&server, crate::config::ServerCredential { cookie: Some(cookie), ..Default::default() });
    store.save()?;
    println!("{} Authenticated. Session saved for {server}.", "✓".green());
    Ok(())
}

/// A device-pairing flow started against a server (not yet approved).
pub struct PairingStart {
    /// Normalized server URL.
    pub server: String,
    pub device_code: String,
    pub user_code: String,
    /// Poll interval in seconds, as suggested by the server.
    pub interval: u64,
    /// Expiry as unix seconds.
    pub expires_at: i64,
}

/// POST /auth/device/start — begin device pairing. `scope` is a suggestion;
/// the approver decides the final scope on the dashboard.
pub async fn start_pairing(server: &str, scope: &str, client_name: &str) -> Result<PairingStart> {
    let server = normalize_server(server);
    let resp = reqwest::Client::new()
        .post(format!("{server}/auth/device/start"))
        .json(&serde_json::json!({ "client_name": client_name, "scope": scope }))
        .send()
        .await
        .context("pairing request failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("failed to start pairing ({}): {}", status, body);
    }
    let start: serde_json::Value = resp.json().await.context("invalid pairing response")?;
    let device_code = start["device_code"].as_str().context("missing device_code")?.to_string();
    let user_code = start["user_code"].as_str().context("missing user_code")?.to_string();
    let expires_in = start["expires_in"].as_i64().unwrap_or(600).max(0);
    let interval = start["interval"].as_i64().unwrap_or(3).max(1) as u64;
    Ok(PairingStart { server, device_code, user_code, interval, expires_at: crate::config::unix_now() + expires_in })
}

/// One POST /auth/device/token poll.
pub enum PairingPoll {
    Pending,
    Approved { token: String, scope: String, project_id: Option<String> },
    Denied,
    Expired,
}

/// Poll a pairing once. Transient network errors and non-2xx (incl. 429)
/// map to Pending — callers keep polling.
pub async fn poll_pairing(client: &reqwest::Client, server: &str, device_code: &str) -> PairingPoll {
    let resp = match client
        .post(format!("{}/auth/device/token", server.trim_end_matches('/')))
        .json(&serde_json::json!({ "device_code": device_code }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return PairingPoll::Pending,
    };
    if !resp.status().is_success() {
        return PairingPoll::Pending;
    }
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    match body["status"].as_str().unwrap_or("pending") {
        "ok" => match body["token"].as_str() {
            Some(token) => PairingPoll::Approved {
                token: token.to_string(),
                scope: body["scope"].as_str().unwrap_or("deploy").to_string(),
                project_id: body["project_id"].as_str().map(str::to_string),
            },
            None => PairingPoll::Pending,
        },
        "denied" => PairingPoll::Denied,
        "expired" => PairingPoll::Expired,
        _ => PairingPoll::Pending,
    }
}

/// Device-pairing login: prints a short-lived code, waits for the user to
/// approve it at `{server}/connect` from an authenticated browser, then stores
/// the issued scoped token. No passwords pass through the terminal.
pub async fn login(server: &str, suggested_scope: &str) -> Result<()> {
    let client_name = std::env::var("L8B_CLIENT_NAME").unwrap_or_else(|_| "l8b CLI".to_string());
    let start = start_pairing(server, suggested_scope, &client_name).await?;
    let server = start.server;
    let user_code = &start.user_code;

    println!("Server: {server}");
    println!();
    let connect_url = format!("{server}/connect?code={user_code}");
    println!("  Open to approve:");
    println!();
    println!("    {}", connect_url.cyan().bold());
    println!();
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        use std::io::Write;
        print!("  Press ENTER to open the browser… ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let _ = webbrowser::open(&connect_url);
        println!();
    }
    let expires_in = (start.expires_at - crate::config::unix_now()).max(0) as u64;
    println!("  {} Expires in {} minutes. Waiting for approval…", "⏳".yellow(), expires_in / 60);

    let client = reqwest::Client::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(start.interval)).await;
        if crate::config::unix_now() >= start.expires_at {
            anyhow::bail!("pairing code expired before approval — run `l8b login` again");
        }

        match poll_pairing(&client, &server, &start.device_code).await {
            PairingPoll::Pending => continue,
            PairingPoll::Denied => anyhow::bail!("pairing was denied on the server"),
            PairingPoll::Expired => anyhow::bail!("pairing code expired — run `l8b login` again"),
            PairingPoll::Approved { token, scope, project_id } => {
                let mut store = CredentialStore::load();
                store.upsert(
                    &server,
                    crate::config::ServerCredential { token: Some(token), scope: Some(scope), ..Default::default() },
                );
                store.save()?;
                println!();
                let scope = store.get(&server).and_then(|c| c.scope.clone()).unwrap_or_default();
                println!("{} Authenticated. Token saved for {server} (scope: {scope}).", "✓".green());
                if let Some(p) = project_id {
                    println!("  Bound to project '{p}'.");
                }
                println!("  {} Revoke anytime from the dashboard: Settings → Access Tokens.", "→".dimmed());
                return Ok(());
            }
        }
    }
}

/// POST to the API using session (cookie) auth.
pub async fn session_post(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let cookie = cookie_for(server)?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .post(&url)
        .header("Cookie", &cookie)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// GET from the API using session (cookie) auth.
pub async fn session_get(client: &reqwest::Client, server: &str, path: &str) -> Result<serde_json::Value> {
    let cookie = cookie_for(server)?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp =
        client.get(&url).header("Cookie", &cookie).send().await.with_context(|| format!("GET {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// DELETE from the API using session (cookie) auth.
pub async fn session_delete(client: &reqwest::Client, server: &str, path: &str) -> Result<serde_json::Value> {
    let cookie = cookie_for(server)?;

    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp =
        client.delete(&url).header("Cookie", &cookie).send().await.with_context(|| format!("DELETE {} failed", url))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));

    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }

    Ok(json)
}

/// GET from the API using the client's baked-in auth (token or session).
pub async fn api_get(client: &reqwest::Client, server: &str, path: &str) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client.get(&url).send().await.with_context(|| format!("GET {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// PUT JSON using the client's baked-in auth.
pub async fn api_put_json(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .with_context(|| format!("PUT {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// POST JSON using the client's baked-in auth.
pub async fn api_post_json(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// PATCH JSON using the client's baked-in auth (token or session).
pub async fn api_patch_json(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client
        .patch(&url)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .with_context(|| format!("PATCH {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// DELETE on the API using the client's baked-in auth (token or session).
pub async fn api_delete(client: &reqwest::Client, server: &str, path: &str) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client.delete(&url).send().await.with_context(|| format!("DELETE {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// POST multipart using the client's baked-in auth (token or session).
pub async fn api_post_multipart(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    form: reqwest::multipart::Form,
) -> Result<serde_json::Value> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let resp = client.post(&url).multipart(form).send().await.with_context(|| format!("POST {} failed", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::json!({"raw": body_text}));
    if !status.is_success() {
        let error = json["error"].as_str().unwrap_or(&body_text);
        anyhow::bail!("{} ({}): {}", url, status, error);
    }
    Ok(json)
}

/// Platform domain for project URLs, from `GET /meta`; derived from the
/// server URL when the server is unreachable.
pub async fn fetch_platform_domain(client: &reqwest::Client, server: &str) -> String {
    if let Ok(meta) = api_get(client, server, "/meta").await
        && let Some(domain) = meta["domain"].as_str()
    {
        let domain = domain.trim();
        if !domain.is_empty() {
            return domain.to_string();
        }
    }
    derive_domain_from_server(server)
}

/// Last-resort domain derivation when /settings is unavailable.
/// `https://dash.11b.in` → `11b.in`
fn derive_domain_from_server(server: &str) -> String {
    let host = server.trim().trim_end_matches('/').trim_start_matches("https://").trim_start_matches("http://");
    let host = host.split('/').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 3 { parts[1..].join(".") } else { host.to_string() }
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
    /// Public IP if the node is directly reachable for uploads (enables direct mode).
    #[serde(default)]
    pub public_ip: Option<String>,
}

/// Fetch online nodes from the server. Returns empty vec on failure.
pub async fn fetch_online_nodes(client: &reqwest::Client, server: &str) -> Vec<NodeInfo> {
    match api_get(client, server, "/nodes").await {
        Ok(resp) => {
            let nodes: Vec<NodeInfo> = serde_json::from_value(resp).unwrap_or_default();
            nodes.into_iter().filter(|n| n.status == NodeStatus::Online).collect()
        }
        Err(_) => Vec::new(),
    }
}

/// Where/how to upload an image. Returned by the master's `/images/upload-target` broker.
#[derive(Debug, Deserialize)]
pub struct UploadTarget {
    /// "local" | "relay" | "direct"
    pub mode: String,
    pub token: String,
    pub chunk_size: u64,
    #[allow(dead_code)]
    pub expires_at: i64,
    /// Present only for direct: agent public base URL (`https://<ip>/__l8b_upload`).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Present only for direct: agent CA PEM the client must trust.
    #[serde(default)]
    pub ca_pem: Option<String>,
}

/// Ask the master where to upload. `mode_hint` is "direct" or "relay" (None = auto).
pub async fn request_upload_target(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    image_id: &str,
    node_id: Option<&str>,
    mode_hint: Option<&str>,
) -> Result<UploadTarget> {
    let mut body = serde_json::json!({
        "project_id": project_id,
        "image_id": image_id,
    });
    if let Some(n) = node_id {
        body["node_id"] = serde_json::Value::String(n.to_string());
    }
    if let Some(m) = mode_hint {
        body["mode"] = serde_json::Value::String(m.to_string());
    }

    let url = format!("{}/images/upload-target", server.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {} failed", url))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let err = json["error"].as_str().unwrap_or(&text);
        anyhow::bail!("{} ({}): {}", url, status, err);
    }
    let target: UploadTarget =
        serde_json::from_str(&text).with_context(|| format!("failed to parse upload-target response: {}", text))?;
    Ok(target)
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::config::{CredentialStore, ServerCredential};

    fn store_with(servers: &[&str]) -> CredentialStore {
        let mut s = CredentialStore::default();
        for url in servers {
            s.upsert(url, ServerCredential { token: Some(format!("t-{url}")), ..Default::default() });
        }
        s
    }

    #[test]
    fn login_hint_audiences() {
        assert_eq!(login_hint(Some("https://a.io"), false), "l8b login --server https://a.io --pair");
        assert_eq!(login_hint(None, false), "l8b login --server <url> --pair");
        assert_eq!(
            login_hint(Some("https://a.io"), true),
            "call the `setup` MCP tool with {\"server\": \"https://a.io\"}"
        );
        assert!(login_hint(None, true).contains("`setup` MCP tool"));
    }

    #[test]
    fn server_resolution_rules() {
        let store = store_with(&["https://a.io", "https://b.io"]);

        // Flag wins and carries that server's own credential.
        let (s, auth) = pick_server(Some("https://b.io"), Some("https://a.io"), None, &store).unwrap();
        assert_eq!(s, "https://b.io");
        assert_eq!(auth, "Bearer t-https://b.io");

        // Flag for an unknown server without a raw token: refusal with the fix.
        let e = pick_server(Some("https://c.io"), None, None, &store).unwrap_err();
        let text = format!("{e:#}");
        assert!(text.contains("not logged in to https://c.io"), "got: {text}");
        assert!(text.contains("l8b login --server https://c.io --pair"));

        // Raw token satisfies an unknown flagged server.
        let (s, auth) = pick_server(Some("https://c.io"), None, Some("raw"), &store).unwrap();
        assert_eq!(s, "https://c.io");
        assert_eq!(auth, "Bearer raw");

        // Toml server beats the store; URLs normalize.
        let (s, _) = pick_server(None, Some("https://b.io/"), None, &store).unwrap();
        assert_eq!(s, "https://b.io");

        // Toml server without a login: refusal naming the repo's server.
        let e = pick_server(None, Some("https://d.io"), None, &store).unwrap_err();
        assert!(format!("{e:#}").contains("this repo deploys to https://d.io"));

        // Multiple logins and no signal: the ambiguity menu.
        let e = pick_server(None, None, None, &store).unwrap_err();
        let text = format!("{e:#}");
        assert!(text.contains("ambiguous server"), "got: {text}");
        assert!(text.contains("https://a.io") && text.contains("https://b.io"));

        // Exactly one login is the honest default.
        let (s, _) = pick_server(None, None, None, &store_with(&["https://solo.io"])).unwrap();
        assert_eq!(s, "https://solo.io");

        // Nothing stored.
        let e = pick_server(None, None, None, &CredentialStore::default()).unwrap_err();
        assert!(format!("{e:#}").contains("not logged in to any server"));
    }

    #[test]
    fn credential_store_round_trips_through_toml() {
        let mut store = CredentialStore::default();
        store.upsert(
            "https://a.io",
            ServerCredential {
                token: Some("secret".into()),
                name: Some("paired".into()),
                scope: Some("deploy".into()),
                ..Default::default()
            },
        );
        store.upsert("https://b.io", ServerCredential { cookie: Some("sid=1".into()), ..Default::default() });

        let text = toml::to_string_pretty(&store).unwrap();
        let parsed: CredentialStore = toml::from_str(&text).unwrap();
        assert_eq!(parsed.get("https://a.io").unwrap().token.as_deref(), Some("secret"));
        assert_eq!(parsed.get("https://a.io").unwrap().auth_header().as_deref(), Some("Bearer secret"));
        assert_eq!(parsed.get("https://b.io").unwrap().auth_header().as_deref(), Some("sid=1"));
        assert_eq!(parsed.default.as_deref(), Some("https://b.io"));
        assert!(parsed.get("https://missing.io").is_none());
    }

    #[test]
    fn pairing_session_round_trips_and_expires() {
        let session = crate::config::PairingSession {
            server: "https://a.io".into(),
            device_code: "dc-1".into(),
            user_code: "L8B-ABC123".into(),
            interval: 5,
            expires_at: crate::config::unix_now() + 600,
        };
        let text = toml::to_string_pretty(&session).unwrap();
        let parsed: crate::config::PairingSession = toml::from_str(&text).unwrap();
        assert_eq!(parsed.device_code, "dc-1");
        assert_eq!(parsed.approval_url(), "https://a.io/connect?code=L8B-ABC123");
        assert!(!parsed.expired());

        let expired = crate::config::PairingSession { expires_at: crate::config::unix_now() - 1, ..session };
        assert!(expired.expired());

        // interval falls back when absent (older file)
        let stripped = text.replace("interval = 5\n", "");
        let parsed: crate::config::PairingSession = toml::from_str(&stripped).unwrap();
        assert_eq!(parsed.interval, 3);
    }
}
