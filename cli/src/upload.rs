use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::AsyncReadExt;

/// Upload a tar file to the orchestrator's /images/upload endpoint.
/// Returns the image_id from the server response.
pub async fn upload_tar(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    tar_path: &Path,
    node_id: Option<&str>,
    ci_mode: bool,
) -> Result<String> {
    upload_tar_inner(client, server, project_id, tar_path, node_id, ci_mode).await
}

async fn upload_tar_inner(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    tar_path: &Path,
    node_id: Option<&str>,
    ci_mode: bool,
) -> Result<String> {
    let file_len = std::fs::metadata(tar_path)?.len();

    let mut url = format!("{}/images/upload?project_id={}", server.trim_end_matches('/'), project_id);
    if let Some(node) = node_id {
        url.push_str(&format!("&node_id={}", node));
    }

    let pb = if !ci_mode {
        let pb = indicatif::ProgressBar::new(file_len);
        pb.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("  {spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_message("Uploading");
        Some(pb)
    } else {
        None
    };

    let mut last_err = None;

    for attempt in 0..3 {
        if attempt > 0 {
            if let Some(pb) = &pb {
                pb.reset();
                pb.set_message(format!("Retrying ({}/{})", attempt + 1, 3));
            }
            if !ci_mode {
                eprintln!("  Upload failed, retrying ({}/{})...", attempt + 1, 3);
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let file = tokio::fs::File::open(tar_path)
            .await
            .with_context(|| format!("failed to open tar file: {}", tar_path.display()))?;

        let progress_pb = pb.clone();
        let stream = async_stream::stream! {
            let mut reader = tokio::io::BufReader::new(file);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(ref pb) = progress_pb {
                            pb.inc(n as u64);
                        }
                        yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[..n]));
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        };

        let body = reqwest::Body::wrap_stream(stream);

        match client
            .post(&url)
            .header("Content-Type", "application/x-tar")
            .body(body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Some(pb) = &pb {
                    pb.finish_and_clear();
                }
                if !ci_mode {
                    println!("  Upload complete");
                }
                let json: serde_json::Value = resp.json().await?;
                if let Some(id) = json["image_id"].as_str().map(|s| s.to_string()) {
                    return Ok(id);
                }
                last_err = Some(anyhow::anyhow!("missing image_id in upload response"));
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                last_err = Some(anyhow::anyhow!("upload failed ({}): {}", status, body));
            }
            Err(e) => {
                last_err = Some(e.into());
            }
        }
    }

    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("upload failed after 3 attempts")))
}
