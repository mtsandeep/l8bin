use serde_json::{Value, json};

/// Returns a Caddy route that redirects HTTP to HTTPS (308 permanent).
/// Returns `None` for localhost domains where the redirect should be skipped.
pub fn http_to_https_redirect(domain: &str) -> Option<Value> {
    if domain.ends_with("localhost") {
        return None;
    }
    Some(json!({
        "match": [{ "protocol": "http" }],
        "handle": [{
            "handler": "static_response",
            "status_code": 308,
            "headers": {
                "Location": ["https://{http.request.host}{http.request.uri}"]
            }
        }]
    }))
}

pub struct CaddyClient {
    admin_url: String,
    client: reqwest::Client,
}

impl CaddyClient {
    pub fn new(admin_url: &str) -> Self {
        Self { admin_url: admin_url.trim_end_matches('/').to_string(), client: reqwest::Client::new() }
    }

    pub fn admin_url(&self) -> &str {
        &self.admin_url
    }

    pub async fn post_json(&self, url: &str, body: &serde_json::Value) -> anyhow::Result<reqwest::Response> {
        let resp = self.client.post(url).header("Content-Type", "application/json").json(body).send().await?;
        Ok(resp)
    }

    /// Check if Caddy admin API is reachable
    pub async fn ping(&self) -> anyhow::Result<()> {
        let url = format!("{}/config/", self.admin_url);
        self.client.get(&url).send().await?;
        Ok(())
    }
}
