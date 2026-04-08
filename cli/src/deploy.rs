use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct DeployResponse {
    pub status: String,
    pub project_id: String,
    pub url: String,
    pub mapped_port: Option<u16>,
}

/// POST /deploy to the orchestrator
pub async fn deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    image: &str,
    port: u16,
    node_id: Option<&str>,
    cmd: Option<&str>,
    memory: Option<i64>,
    cpu: Option<f64>,
    auto_stop_enabled: bool,
) -> Result<DeployResponse> {
    let url = format!("{}/deploy", server.trim_end_matches('/'));

    let mut body = serde_json::json!({
        "project_id": project_id,
        "image": image,
        "port": port,
        "auto_stop_enabled": auto_stop_enabled,
    });

    if let Some(node) = node_id {
        body["node_id"] = serde_json::json!(node);
    }
    if let Some(c) = cmd {
        body["cmd"] = serde_json::json!(c);
    }
    if let Some(m) = memory {
        body["memory_limit_mb"] = serde_json::json!(m);
    }
    if let Some(c) = cpu {
        body["cpu_limit"] = serde_json::json!(c);
    }

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("deploy request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("deploy failed ({}): {}", status, body_text);
    }

    resp.json().await.context("failed to parse deploy response")
}
