use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::MAX_RETRIES;

/// Fallback mise version if we can't fetch from Railpack source.
/// As of Railpack v0.23.0 the actual version is 2026.3.17.
const FALLBACK_MISE_VERSION: &str = "2026.3.17";

/// Fetch the mise version that Railpack expects by reading version.txt from the
/// Railpack source at the given release tag.
/// Falls back to FALLBACK_MISE_VERSION on any failure.
pub async fn fetch_mise_version(railpack_tag: &str) -> String {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    else {
        return FALLBACK_MISE_VERSION.to_string();
    };

    let url = format!(
        "{}/{}/core/mise/version.txt",
        crate::config::RAILPACK_SOURCE_BASE,
        railpack_tag
    );

    let Ok(resp) = client
        .get(&url)
        .header("User-Agent", "l8b-cli")
        .send()
        .await
    else {
        return FALLBACK_MISE_VERSION.to_string();
    };

    if let Ok(text) = resp.text().await {
        let version = text.trim().to_string();
        if !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return version;
        }
    }

    FALLBACK_MISE_VERSION.to_string()
}

/// Railpack's hardcoded mise install directory — see railwayapp/railpack core/mise/mise.go InstallDir.
const MISE_INSTALL_DIR: &str = "/tmp/railpack/mise";

/// Ensure mise is installed where Railpack expects it (Linux/macOS native only).
pub async fn ensure_mise_for_railpack(railpack_tag: &str, ci_mode: bool) -> Result<()> {
    let mise_version = fetch_mise_version(railpack_tag).await;
    let binary_name = format!("mise-{}", mise_version);
    let install_path = PathBuf::from(MISE_INSTALL_DIR).join(&binary_name);

    if install_path.exists() && install_path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(());
    }

    if !ci_mode { println!("  {} Setting up mise for Railpack...", "::".dimmed()); }

    for attempt in 1..=MAX_RETRIES {
        match download_and_install(&mise_version, &binary_name, ci_mode).await {
            Ok(()) => {
                if !ci_mode { println!("  {} mise v{} installed", "✔".green(), mise_version); }
                return Ok(());
            }
            Err(e) => {
                if attempt < MAX_RETRIES {
                    if !ci_mode { println!("  {} Download failed (attempt {}/{}): {}", "!".yellow(), attempt, MAX_RETRIES, e); }
                } else {
                    anyhow::bail!("mise download failed after {} attempts: {}", MAX_RETRIES, e);
                }
            }
        }
    }

    unreachable!()
}

fn asset_name(version: &str) -> String {
    let (os, arch) = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            ("macos", "arm64")
        } else {
            ("macos", "x64")
        }
    } else if cfg!(target_arch = "aarch64") {
        ("linux", "arm64")
    } else {
        ("linux", "x64")
    };

    format!("mise-v{version}-{os}-{arch}.tar.gz")
}

async fn download_and_install(version: &str, binary_name: &str, ci_mode: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let name = asset_name(version);
    let download_url = format!(
        "{}/v{version}/{name}",
        crate::config::MISE_RELEASE_BASE,
    );

    if !ci_mode { println!("  {} Downloading mise v{}...", "::".dimmed(), version); }
    let bytes = client
        .get(&download_url)
        .header("User-Agent", "l8b-cli")
        .send()
        .await
        .context("failed to download mise")?
        .bytes()
        .await
        .context("failed to read mise download")?;

    let temp_dir = std::env::temp_dir();
    let archive_path = temp_dir.join(&name);
    std::fs::write(&archive_path, &bytes)?;

    let extract_dir = temp_dir.join("mise-extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;

    let status = std::process::Command::new("tar")
        .args(["xzf", &archive_path.to_string_lossy(), "-C", &extract_dir.to_string_lossy()])
        .status()?;

    if !status.success() {
        let _ = std::fs::remove_file(&archive_path);
        let _ = std::fs::remove_dir_all(&extract_dir);
        anyhow::bail!("failed to extract mise archive");
    }

    let extracted_bin = find_file(&extract_dir, "mise")
        .context("mise binary not found in archive")?;

    let install_dir = PathBuf::from(MISE_INSTALL_DIR);
    std::fs::create_dir_all(&install_dir)?;
    let dest = install_dir.join(binary_name);
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    std::fs::copy(&extracted_bin, &dest)?;

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);

    Ok(())
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
