//! GitHub App Manifest flow: build a prefilled app definition (POSTed to
//! github.com/settings/apps/new as the documented `manifest` form field) and
//! exchange the callback code for the app's credentials (id, slug, client
//! secret, private key, webhook secret).

use serde::Deserialize;

use super::{API_BASE, GitHubApiError, USER_AGENT};

/// Manifest keys are strict: anything outside the documented set (name, url,
/// hook_attributes, redirect_url, callback_urls, setup_url, description,
/// public, default_events, default_permissions, request_oauth_on_install,
/// setup_on_update) makes GitHub silently drop the whole prefill.
///
/// Permissions the deploy flow needs:
/// - metadata:read  — repo browsing (implicit baseline, declared for clarity)
/// - contents:write — commit the generated workflow file
/// - workflows:write— author files under .github/workflows/ (OAuth apps can't)
/// - actions:write  — dispatch workflows, read runs
/// - secrets:write  — write L8B_SERVER / L8B_TOKEN repo secrets
///
/// No `default_events`: `installation` (un)install events are delivered to
/// Apps automatically, and repo add/remove is covered by live polling of
/// GET /app/installations. Subscribing explicitly fails validation.
pub fn manifest_json(name: &str, dashboard_url: &str, webhook_override: Option<&str>) -> serde_json::Value {
    let mut manifest = serde_json::json!({
        "name": name,
        "url": dashboard_url,
        "redirect_url": format!("{dashboard_url}/settings/github/callback"),
        "public": false,
        "request_oauth_on_install": false,
        "setup_url": format!("{dashboard_url}/settings/github"),
        "callback_urls": [],
        "default_permissions": {
            "metadata": "read",
            "contents": "write",
            "workflows": "write",
            "actions": "write",
            "secrets": "write"
        }
    });
    // An explicit override wins (the user chose it in the advanced view);
    // otherwise GitHub validates webhook reachability at registration —
    // local-only dashboards (localhost / private IPs) register without one.
    // Installation status falls back to polling /github/installations, which
    // is already what the panel and callback page drive.
    let webhook_url = webhook_override
        .map(str::to_string)
        .or_else(|| webhook_reachable(dashboard_url).then(|| format!("{dashboard_url}/github/webhook")));
    if let Some(url) = webhook_url {
        manifest["hook_attributes"] = serde_json::json!({ "url": url, "active": true });
    }
    manifest
}

/// Over-broad on purpose: omitting the webhook is harmless (polling covers
/// it), registering with an unreachable one is a hard GitHub validation error.
fn webhook_reachable(dashboard_url: &str) -> bool {
    let host = dashboard_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || !host.contains('.') {
        return false;
    }
    // sslip.io/nip.io style hosts embed the IP mid-host (l8b.127.0.0.1.sslip.io),
    // so scan label windows for a private/loopback IPv4 rather than the first label.
    let labels: Vec<&str> = host.split('.').collect();
    let private_ip = labels.windows(4).any(|w| {
        let octets: Vec<u8> = w.iter().filter_map(|l| l.parse::<u8>().ok()).collect();
        if octets.len() != 4 {
            return false;
        }
        octets[0] == 127
            || octets[0] == 10
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
            || (octets[0] == 169 && octets[1] == 254)
    });
    !private_ip
}

#[derive(Debug, Deserialize)]
pub struct ConvertedApp {
    pub id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub pem: String,
    pub webhook_secret: Option<String>,
}

/// Exchange the single-use manifest code for the app credentials.
/// Note the plural path: /app-manifests/ — the singular form is a 404.
pub async fn exchange_code(code: &str) -> Result<ConvertedApp, GitHubApiError> {
    let resp = reqwest::Client::new()
        .post(format!("{API_BASE}/app-manifests/{code}/conversions"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GitHubApiError::from_response(status, &body));
    }
    Ok(resp.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key must be in GitHub's documented manifest schema — an unknown
    /// key (e.g. `hook_url` instead of `hook_attributes`) makes GitHub render
    /// the registration form with NO prefill.
    #[test]
    fn manifest_uses_only_documented_keys() {
        let m = manifest_json("l8bin", "https://l8b.example.com", None);
        let allowed = [
            "name",
            "url",
            "hook_attributes",
            "redirect_url",
            "callback_urls",
            "setup_url",
            "description",
            "public",
            "default_events",
            "default_permissions",
            "request_oauth_on_install",
            "setup_on_update",
        ];
        for key in m.as_object().unwrap().keys() {
            assert!(allowed.contains(&key.as_str()), "undocumented manifest key: {key}");
        }
    }

    #[test]
    fn webhook_uses_hook_attributes() {
        let m = manifest_json("l8bin", "https://l8b.example.com", None);
        assert_eq!(m["hook_attributes"]["url"], "https://l8b.example.com/github/webhook");
        assert_eq!(m["hook_attributes"]["active"], true);
        assert!(m.get("hook_url").is_none());
        assert!(m.get("default_events").is_none());
        assert_eq!(m["redirect_url"], "https://l8b.example.com/settings/github/callback");
        assert_eq!(m["default_permissions"]["workflows"], "write");
    }

    #[test]
    fn local_dashboards_register_without_webhook() {
        for url in [
            "https://l8b.localhost",
            "https://l8b.127.0.0.1.sslip.io",
            "https://l8b.192.168.1.10.nip.io",
            "https://10.0.0.5:5088",
        ] {
            let m = manifest_json("l8bin", url, None);
            assert!(m.get("hook_attributes").is_none(), "expected no webhook for {url}");
            // redirect_url survives — the browser callback is what matters locally.
            assert!(m["redirect_url"].as_str().unwrap().starts_with(url));
        }
    }

    #[test]
    fn webhook_override_wins_even_on_localhost() {
        let m = manifest_json("l8bin", "https://l8b.localhost", Some("https://events.example.com/hook"));
        assert_eq!(m["hook_attributes"]["url"], "https://events.example.com/hook");
    }
}
