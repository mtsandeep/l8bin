use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Per-invocation overrides (CLI flags > env vars). Stored credentials live
/// in `CredentialStore`.
#[derive(Debug, Clone, Default)]
pub struct CliConfig {
    pub server: Option<String>,
    pub token: Option<String>,
}

impl CliConfig {
    pub fn load(cli_server: Option<&str>, cli_token: Option<&str>) -> Self {
        Self {
            server: cli_server.map(str::to_string).or_else(|| std::env::var("L8B_SERVER").ok()),
            token: cli_token.map(str::to_string).or_else(|| std::env::var("L8B_TOKEN").ok()),
        }
    }
}

pub const APP_DIR: &str = "litebin";
const CONFIG_FILE: &str = "config.toml";
const PAIRING_FILE: &str = "pairing.toml";

/// Wall-clock unix seconds (monotonicity is not needed here).
pub fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

/// Railpack GitHub release URL (used to auto-download the binary)
pub const RAILPACK_RELEASE_URL: &str = "https://api.github.com/repos/railwayapp/railpack/releases/latest";

/// Base URL for Railpack source files (version.txt, install.go)
pub const RAILPACK_SOURCE_BASE: &str = "https://raw.githubusercontent.com/railwayapp/railpack";

/// Base URL for Railpack GitHub releases (binary downloads)
pub const RAILPACK_RELEASE_BASE: &str = "https://github.com/railwayapp/railpack/releases/download";

/// Base URL for mise GitHub releases (binary downloads)
pub const MISE_RELEASE_BASE: &str = "https://github.com/jdx/mise/releases/download";

/// Docker image tag for the Railpack frontend container (Windows)
pub const RAILPACK_IMAGE: &str = "l8b-railpack:latest";

/// Docker image tag prefix
pub const IMAGE_PREFIX: &str = "l8b";

/// Max retries for network-dependent operations (downloads, builds)
pub const MAX_RETRIES: u32 = 3;

/// Credential for one server: a pairing/token auth, or a session cookie.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Token name (from pairing), for display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
}

impl ServerCredential {
    pub fn auth_header(&self) -> Option<String> {
        if let Some(ref t) = self.token { Some(format!("Bearer {t}")) } else { self.cookie.clone() }
    }
}

/// All stored logins, keyed by normalized server URL. `default` marks the
/// last one logged into.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialStore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerCredential>,
}

pub fn normalize_server(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") { s.to_string() } else { format!("https://{s}") }
}

impl CredentialStore {
    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(APP_DIR).join(CONFIG_FILE)
    }

    pub fn load() -> Self {
        let path = Self::path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        match toml::from_str(&content) {
            Ok(store) => store,
            Err(e) => {
                eprintln!("warning: invalid {} ({e}); treating as empty — re-login with l8b login", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn get(&self, server: &str) -> Option<&ServerCredential> {
        self.servers.get(&normalize_server(server)).filter(|c| c.auth_header().is_some())
    }

    pub fn upsert(&mut self, server: &str, credential: ServerCredential) {
        self.default = Some(normalize_server(server));
        self.servers.insert(normalize_server(server), credential);
    }

    pub fn remove(&mut self, server: &str) {
        let key = normalize_server(server);
        self.servers.remove(&key);
        if self.default.as_deref() == Some(key.as_str()) {
            self.default = self.servers.keys().next().cloned();
        }
    }

    /// Every server with a usable credential.
    pub fn logged_in_servers(&self) -> Vec<&String> {
        self.servers.iter().filter(|(_, c)| c.auth_header().is_some()).map(|(s, _)| s).collect()
    }

    /// Show: redacted in CI mode, raw file otherwise.
    pub fn show(&self, ci_enabled: bool) -> Result<()> {
        let path = Self::path();
        if self.servers.is_empty() {
            println!("No logins stored. Add one with:");
            println!("  l8b login --server <url> --pair");
            return Ok(());
        }
        if ci_enabled {
            for s in self.logged_in_servers() {
                let marker = if self.default.as_deref() == Some(s.as_str()) { " (default)" } else { "" };
                println!("server: {s}{marker}");
                println!("  auth: token (set)");
            }
        } else {
            println!("{}", std::fs::read_to_string(&path).unwrap_or_default());
        }
        Ok(())
    }
}

/// An in-flight device pairing, persisted between `setup` tool calls — the
/// MCP process may restart between starting and completing one. A single
/// session exists at a time; starting a new one overwrites it, and the
/// superseded code lapses server-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingSession {
    /// Normalized server URL.
    pub server: String,
    pub device_code: String,
    pub user_code: String,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
    /// Unix seconds.
    pub expires_at: i64,
}

fn default_poll_interval() -> u64 {
    3
}

impl PairingSession {
    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(APP_DIR).join(PAIRING_FILE)
    }

    /// None when missing or invalid — an unreadable session is as good as none.
    pub fn load() -> Option<Self> {
        toml::from_str(&std::fs::read_to_string(Self::path()).ok()?).ok()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Best-effort removal — called on success, denial, and expiry.
    pub fn clear() {
        std::fs::remove_file(Self::path()).ok();
    }

    pub fn expired(&self) -> bool {
        unix_now() >= self.expires_at
    }

    pub fn approval_url(&self) -> String {
        format!("{}/connect?code={}", self.server.trim_end_matches('/'), self.user_code)
    }
}
