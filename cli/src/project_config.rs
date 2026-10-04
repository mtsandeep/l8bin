//! `l8b.toml` — per-repo project defaults so agents discover "this deploys via
//! LiteBin" without being told. Written by `l8b init`, read by commands as
//! fallbacks for explicit flags.

use anyhow::{Result, bail};
use serde::Deserialize;
use std::path::Path;

pub const FILE_NAME: &str = "l8b.toml";

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ProjectConfig {
    pub project: Option<String>,
    pub port: Option<u16>,
    pub node: Option<String>,
    pub env_file: Option<String>,
}

pub fn load(dir: &Path) -> Option<ProjectConfig> {
    let raw = std::fs::read_to_string(dir.join(FILE_NAME)).ok()?;
    match toml::from_str(&raw) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("warning: invalid {FILE_NAME}: {e}");
            None
        }
    }
}

/// Explicit flag wins; then l8b.toml; then a clear error with the fix.
pub fn resolve_project(explicit: Option<&str>, dir: &Path) -> Result<String> {
    if let Some(p) = explicit.filter(|s| !s.is_empty()) {
        return Ok(p.to_string());
    }
    if let Some(p) = load(dir).and_then(|c| c.project) {
        return Ok(p);
    }
    bail!("no project specified — pass it explicitly or set `project` in {FILE_NAME} (run `l8b init`)");
}
