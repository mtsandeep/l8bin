use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    pub server: Option<String>,
    pub token: Option<String>,
}

pub const APP_DIR: &str = "litebin";
const CONFIG_FILE: &str = "config.toml";

/// Railpack GitHub release URL (used to auto-download the binary)
pub const RAILPACK_RELEASE_URL: &str =
    "https://api.github.com/repos/railwayapp/railpack/releases/latest";

/// Base URL for Railpack source files (version.txt, install.go)
pub const RAILPACK_SOURCE_BASE: &str =
    "https://raw.githubusercontent.com/railwayapp/railpack";

/// Base URL for Railpack GitHub releases (binary downloads)
pub const RAILPACK_RELEASE_BASE: &str =
    "https://github.com/railwayapp/railpack/releases/download";

/// Base URL for mise GitHub releases (binary downloads)
pub const MISE_RELEASE_BASE: &str =
    "https://github.com/jdx/mise/releases/download";

/// Docker image tag for the Railpack frontend container (Windows)
pub const RAILPACK_IMAGE: &str = "l8b-railpack:latest";

/// Docker image tag prefix
pub const IMAGE_PREFIX: &str = "l8b";

/// Max retries for network-dependent operations (downloads, builds)
pub const MAX_RETRIES: u32 = 3;

impl CliConfig {
    /// Load config from: CLI args > env vars > config file
    pub fn load(
        cli_server: Option<&str>,
        cli_token: Option<&str>,
    ) -> Result<Self> {
        let file_config = Self::read_config_file().unwrap_or_default();

        let server = cli_server
            .map(|s| s.to_string())
            .or_else(|| std::env::var("L8B_SERVER").ok())
            .or(file_config.server.clone());

        let token = cli_token
            .map(|s| s.to_string())
            .or_else(|| std::env::var("L8B_TOKEN").ok())
            .or(file_config.token.clone());

        Ok(Self { server, token })
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_DIR)
            .join(CONFIG_FILE)
    }

    fn read_config_file() -> Option<CliConfig> {
        let path = Self::config_path();
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }

    pub fn save(server: Option<&str>, token: Option<&str>) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut config = Self::read_config_file().unwrap_or_default();
        if let Some(s) = server {
            config.server = Some(s.to_string());
        }
        if let Some(t) = token {
            config.token = Some(t.to_string());
        }

        let content = toml::to_string_pretty(&config)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

}
