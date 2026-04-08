use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::MAX_RETRIES;

/// Return the path to the Railpack binary and its version tag,
/// downloading it if not cached. (Linux / macOS native only — Windows uses Docker.)
pub async fn ensure_railpack() -> Result<(PathBuf, String)> {
    let bin_path = railpack_bin_path();

    if bin_path.exists() && bin_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        let version = fetch_latest_tag().await.unwrap_or_else(|_| "v0.23.0".into());
        return Ok((bin_path, version));
    }

    println!("  {} Railpack not found, downloading...", "::".dimmed());

    for attempt in 1..=MAX_RETRIES {
        match download_railpack(&bin_path).await {
            Ok(version) => {
                println!("  {} Railpack {} installed", "✔".green(), version);
                return Ok((bin_path, version));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&bin_path);
                if attempt < MAX_RETRIES {
                    println!("  {} Download failed (attempt {}/{}): {}", "!".yellow(), attempt, MAX_RETRIES, e);
                } else {
                    anyhow::bail!("Railpack download failed after {} attempts: {}", MAX_RETRIES, e);
                }
            }
        }
    }

    unreachable!()
}

async fn fetch_latest_tag() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp: serde_json::Value = client
        .get(crate::config::RAILPACK_RELEASE_URL)
        .header("User-Agent", "l8b-cli")
        .send()
        .await?
        .json()
        .await?;
    Ok(resp["tag_name"].as_str().unwrap_or("v0.23.0").to_string())
}

fn railpack_bin_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::config::APP_DIR)
        .join("bin")
        .join("railpack")
}

fn asset_name(version: &str) -> String {
    let version = version.strip_prefix('v').unwrap_or(version);
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    let os = if cfg!(target_os = "macos") { "apple-darwin" } else { "unknown-linux-musl" };
    format!("railpack-v{version}-{arch}-{os}.tar.gz")
}

async fn download_railpack(bin_path: &Path) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let resp: serde_json::Value = client
        .get(crate::config::RAILPACK_RELEASE_URL)
        .header("User-Agent", "l8b-cli")
        .send()
        .await
        .context("failed to fetch Railpack release info")?
        .json()
        .await
        .context("failed to parse Railpack release info")?;

    let version = resp["tag_name"].as_str().unwrap_or("v0.23.0");
    let name = asset_name(version);

    let empty: Vec<serde_json::Value> = vec![];
    let assets = resp["assets"].as_array().unwrap_or(&empty);
    let download_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(name.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| anyhow::anyhow!("no Railpack release found for your platform ({name})"))?;

    let temp_dir = std::env::temp_dir();
    let archive_path = temp_dir.join(&name);

    println!("  {} Downloading Railpack {}...", "::".dimmed(), version);
    let bytes = client
        .get(download_url)
        .header("User-Agent", "l8b-cli")
        .send()
        .await
        .context("failed to download Railpack")?
        .bytes()
        .await
        .context("failed to read Railpack download")?;
    std::fs::write(&archive_path, &bytes)?;

    let extract_dir = temp_dir.join("railpack-extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;

    let status = std::process::Command::new("tar")
        .args(["xzf", &archive_path.to_string_lossy(), "-C", &extract_dir.to_string_lossy()])
        .status()?;

    if !status.success() {
        let _ = std::fs::remove_file(&archive_path);
        let _ = std::fs::remove_dir_all(&extract_dir);
        anyhow::bail!("failed to extract Railpack archive");
    }

    let extracted_bin = find_file(&extract_dir, "railpack")
        .context("railpack binary not found in archive")?;

    if let Some(parent) = bin_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&extracted_bin, bin_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);

    Ok(version.to_string())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = find_file(&path, name) {
                    return Some(found);
                }
            }
        }
    }
    None
}
