//! Universal chunked, resumable image upload.
//!
//! Replaces the old single-stream POST. The client always asks the master for an
//! upload target (`/images/upload-target`), then chunks the tar to `{base}/{token}/...`.
//! For local/relay uploads the base is the master; for direct uploads it is the
//! agent's public URL and a CA-trust client is used. A dropped chunk retries
//! automatically; a terminal failure offers a manual retry that resumes from the
//! last chunk the server confirmed — the image is never rebuilt.

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::HumanBytes;
use litebin_common::upload::{
    chunk_url, commit_url, status_url, AGENT_UPLOAD_PREFIX, MASTER_UPLOAD_PREFIX, TOTAL_CHUNKS_HEADER,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::Path;

use crate::auth::{self, UploadTarget};

/// How the client prefers to upload. The broker makes the final call (e.g. a node
/// without a public IP falls back to relay regardless).
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub enum UploadMode {
    /// Let the broker decide (direct when the node supports it, else relay/local).
    Auto,
    Direct,
    Relay,
}

impl UploadMode {
    fn hint(self) -> Option<&'static str> {
        match self {
            UploadMode::Auto => None,
            UploadMode::Direct => Some("direct"),
            UploadMode::Relay => Some("relay"),
        }
    }
}

/// High-level entry: ask the master where to upload, build the right client for
/// that target, and run the chunked upload. Returns the resolved image id.
pub async fn upload_image(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    tar_path: &Path,
    image_id: &str,
    node_id: Option<&str>,
    mode: UploadMode,
    ci_mode: bool,
) -> Result<String> {
    let target = auth::request_upload_target(client, server, project_id, image_id, node_id, mode.hint()).await?;

    // For direct mode the broker returns base_url = https://<ip>/__l8b_upload.
    // We connect using the hostname `agent` (matches the cert's SAN=DNS:agent) and
    // pin it to that IP via reqwest's resolve, so SNI=`agent` is sent (Caddy serves
    // the matching cert). Non-direct modes talk to the master normally.
    let (upload_client, base) = if target.mode == "direct" {
        let ca_pem = target
            .ca_pem
            .as_deref()
            .context("direct upload target did not include ca_pem")?;
        let base_url = target
            .base_url
            .as_deref()
            .context("direct upload target did not include base_url")?;
        let url = reqwest::Url::parse(base_url).context("invalid direct base_url")?;
        let host = url.host_str().context("missing host in base_url")?;
        let ip: IpAddr = host
            .parse()
            .with_context(|| format!("direct upload base_url host is not an IP address: '{host}'"))?;
        let client = crate::tls::direct_upload_client(ca_pem, ip)?;
        let base = format!("https://{}{}", crate::tls::DIRECT_HOST, AGENT_UPLOAD_PREFIX);
        (client, base)
    } else {
        (
            client.clone(),
            format!("{}{}", server.trim_end_matches('/'), MASTER_UPLOAD_PREFIX),
        )
    };

    if !ci_mode {
        let label = match target.mode.as_str() {
            "direct" => "direct to agent",
            "relay" => "via master (relay)",
            _ => "to master",
        };
        eprintln!("  {} Uploading {} ({})", "↻".dimmed(), label, tar_path.display());
    }

    chunked_upload(&upload_client, &base, &target, tar_path, ci_mode).await
}

/// Read the bytes of chunk `index` (0-based) from the file.
fn read_chunk(path: &Path, index: u64, chunk_size: usize) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open tar file: {}", path.display()))?;
    let offset = index
        .checked_mul(chunk_size as u64)
        .context("chunk offset overflow")?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; chunk_size];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// The chunk loop. Talks to either master or agent (same protocol). Resumes from
/// the server-confirmed chunk set; per-chunk auto-retry; terminal failure offers
/// a manual retry that resumes (no rebuild).
async fn chunked_upload(
    client: &reqwest::Client,
    base: &str,
    target: &UploadTarget,
    tar_path: &Path,
    ci_mode: bool,
) -> Result<String> {
    use indicatif::{ProgressBar, ProgressStyle};

    let file_size = std::fs::metadata(tar_path)?.len();
    let chunk_size = target.chunk_size as usize;
    let total_chunks = file_size.div_ceil(chunk_size as u64);

    let pb = if !ci_mode {
        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  {spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_message("Uploading");
        Some(pb)
    } else {
        None
    };

    loop {
        // Seed the bar from the server-confirmed received set (never falsely 0).
        let received = fetch_status(client, &base, &target.token).await.unwrap_or_default();
        let mut received_bytes: u64 = received.iter().map(|&i| chunk_bytes(i, chunk_size, file_size)).sum();
        if let Some(pb) = &pb {
            pb.set_position(received_bytes);
        }

        let mut last_err: Option<anyhow::Error> = None;

        for index in 0..total_chunks {
            if received.contains(&index) {
                continue;
            }
            match post_chunk_with_retry(client, &base, &target.token, index, total_chunks, tar_path, chunk_size).await {
                Ok(n) => {
                    received_bytes = received_bytes.saturating_add(n).min(file_size);
                    if let Some(pb) = &pb {
                        pb.set_position(received_bytes);
                    }
                }
                Err(e) => {
                    if !ci_mode {
                        eprintln!("  {} chunk {} failed: {}", "!".yellow(), index, e);
                    }
                    last_err = Some(e);
                    break;
                }
            }
        }

        if last_err.is_none() {
            match commit(client, &base, &target.token).await {
                Ok(image_id) => {
                    if let Some(pb) = &pb {
                        pb.finish_and_clear();
                    }
                    if !ci_mode {
                        println!("  {} Upload complete", "✓".green());
                    }
                    return Ok(image_id);
                }
                Err(e) => last_err = Some(e),
            }
        }

        // Terminal failure → manual retry (resume from confirmed chunks, no rebuild).
        let err_msg = last_err.as_ref().map(|e| e.to_string()).unwrap_or_else(|| "unknown error".to_string());
        let at_bytes = received_bytes;

        if ci_mode {
            if let Some(pb) = &pb {
                pb.finish_and_clear();
            }
            return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("upload failed")));
        }

        if let Some(pb) = &pb {
            pb.set_message(format!("failed at {}", HumanBytes(at_bytes)));
        }
        let retry = {
            let prompt_msg = format!(
                "Upload failed at {} ({}). Retry from the last received chunk? (the image won't be rebuilt)",
                HumanBytes(at_bytes),
                err_msg
            );
            let pb_ref = &pb;
            match pb_ref {
                Some(pb) => pb.suspend(|| {
                    dialoguer::Confirm::new().with_prompt(prompt_msg).default(true).interact()
                }),
                None => dialoguer::Confirm::new().with_prompt(prompt_msg).default(true).interact(),
            }
            .map_err(|e| anyhow::anyhow!("failed to read retry response: {}", e))?
        };
        if !retry {
            if let Some(pb) = &pb {
                pb.finish_and_clear();
            }
            return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("upload failed")));
        }
        if let Some(pb) = &pb {
            pb.set_message("Uploading");
        }
    }
}

/// Byte length of chunk `index`, accounting for the (shorter) final chunk.
fn chunk_bytes(index: u64, chunk_size: usize, file_size: u64) -> u64 {
    let start = index * chunk_size as u64;
    if start >= file_size {
        return 0;
    }
    std::cmp::min(chunk_size as u64, file_size - start)
}

async fn fetch_status(client: &reqwest::Client, base: &str, token: &str) -> Result<HashSet<u64>> {
    let url = status_url(base, token);
    let resp = client.get(&url).send().await.context("status request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("status {} failed: {}", url, resp.status());
    }
    let v: serde_json::Value = resp.json().await.context("status response parse failed")?;
    let set = v["received"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|x| x.as_u64()).collect())
        .unwrap_or_default();
    Ok(set)
}

/// POST one chunk with up to 3 automatic attempts. Returns the byte count on success.
async fn post_chunk_with_retry(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    index: u64,
    total: u64,
    tar_path: &Path,
    chunk_size: usize,
) -> Result<u64> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..3u8 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let chunk = read_chunk(tar_path, index, chunk_size)?;
        let n = chunk.len() as u64;
        match post_chunk(client, base, token, index, total, chunk).await {
            Ok(()) => return Ok(n),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("chunk {index} upload failed")))
}

async fn post_chunk(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    index: u64,
    total: u64,
    chunk: Vec<u8>,
) -> Result<()> {
    let url = chunk_url(base, token, index);
    let resp = client
        .post(&url)
        .header("Content-Type", "application/octet-stream")
        .header(TOTAL_CHUNKS_HEADER, total)
        .body(chunk)
        .send()
        .await
        .with_context(|| format!("chunk {index} request failed"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("chunk {index} failed ({status}): {body}");
    }
    Ok(())
}

async fn commit(client: &reqwest::Client, base: &str, token: &str) -> Result<String> {
    let url = commit_url(base, token);
    let resp = client.post(&url).send().await.context("commit request failed")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("commit failed ({status}): {}", v["error"].as_str().unwrap_or(&body));
    }
    v["image_id"]
        .as_str()
        .map(|s| s.to_string())
        .with_context(|| format!("missing image_id in commit response (status {status}, body: {body})"))
}
