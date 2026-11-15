//! `/version/latest` — the latest published LiteBin release, checked against
//! GitHub with a one-hour in-memory cache so every dashboard visit shares one
//! API call. Network failures degrade to `latest_version: null` instead of
//! erroring — the About page shows "couldn't check" and the releases link
//! still works.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::github::{API_BASE, USER_AGENT, http};

pub const REPO: &str = "mtsandeep/l8bin";
pub const RELEASES_URL: &str = "https://github.com/mtsandeep/l8bin/releases";
const CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Serialize, utoipa::ToSchema)]
pub struct LatestVersionResponse {
    /// Latest published release, e.g. `0.4.27` — `null` when no check has
    /// succeeded yet.
    pub latest_version: Option<String>,
    /// Release page for the latest version; the releases list when unknown.
    pub release_url: String,
    /// Unix seconds of the last successful check; `null` when none succeeded.
    pub checked_at: Option<i64>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Clone)]
struct CachedRelease {
    version: Option<String>,
    release_url: String,
    checked_at: i64,
    fetched_at: Instant,
}

static CACHE: LazyLock<Mutex<Option<CachedRelease>>> = LazyLock::new(|| Mutex::new(None));

/// Strip the `v` prefix from a release tag: `v0.4.27` → `0.4.27`.
fn tag_to_version(tag: &str) -> String {
    let tag = tag.trim();
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

async fn fetch_latest() -> Option<CachedRelease> {
    let release: GithubRelease = http()
        .get(format!("{API_BASE}/repos/{REPO}/releases/latest"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let release_url = if release.html_url.is_empty() { RELEASES_URL.to_string() } else { release.html_url };
    Some(CachedRelease {
        version: Some(tag_to_version(&release.tag_name)),
        release_url,
        checked_at: chrono::Utc::now().timestamp(),
        fetched_at: Instant::now(),
    })
}

#[utoipa::path(
    get,
    path = "/version/latest",
    description = "Latest published LiteBin release (GitHub check, cached one hour). latest_version is null when the check could not run.",
    responses(
        (status = 200, description = "Latest release info", body = LatestVersionResponse),
    ),
    tag = "health",
    security(
        ("session_auth" = []),
    ),
)]
pub async fn latest() -> Json<LatestVersionResponse> {
    // Fresh cache serves immediately; otherwise refresh without holding the
    // lock across the request (a concurrent double fetch is harmless).
    let fresh = {
        let cache = CACHE.lock().unwrap();
        cache.as_ref().is_some_and(|c| c.fetched_at.elapsed() < CACHE_TTL)
    };
    if !fresh {
        let fetched = fetch_latest().await;
        let mut cache = CACHE.lock().unwrap();
        // On failure keep serving a stale entry rather than an empty answer.
        if let Some(fetched) = fetched {
            *cache = Some(fetched);
        }
    }
    let cache = CACHE.lock().unwrap();
    match cache.clone() {
        Some(c) => Json(LatestVersionResponse {
            latest_version: c.version,
            release_url: c.release_url,
            checked_at: Some(c.checked_at),
        }),
        None => Json(LatestVersionResponse {
            latest_version: None,
            release_url: RELEASES_URL.to_string(),
            checked_at: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::tag_to_version;

    #[test]
    fn strips_v_prefix() {
        assert_eq!(tag_to_version("v0.4.27"), "0.4.27");
        assert_eq!(tag_to_version("0.4.27"), "0.4.27");
        assert_eq!(tag_to_version("  v1.0.0  "), "1.0.0");
        assert_eq!(tag_to_version(""), "");
    }
}
