//! `l8b.toml` — per-repo project defaults so agents discover "this deploys via
//! LiteBin" without being told. Written by `l8b init`, updated by `l8b deploy`.
//! Non-secret facts only (project, node); env values live server-side.

use anyhow::{Result, bail};
use serde::Deserialize;
use std::path::Path;

pub const FILE_NAME: &str = "l8b.toml";

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ProjectConfig {
    pub project: Option<String>,
    pub node: Option<String>,
    pub server: Option<String>,
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
        return Ok(p.to_string());
    }
    bail!("no project specified — pass it explicitly or set `project` in {FILE_NAME} (run `l8b init`)");
}

/// Record a successful deploy in `l8b.toml` so later sessions redeploy the
/// same project without being told. Writes `project`, `node`, and `server`;
/// returns false when nothing changed.
pub fn record_deploy(dir: &Path, project: &str, node: Option<&str>, server: Option<&str>) -> Result<bool> {
    let existing = load(dir).unwrap_or_default();
    let node = node.map(str::to_string).or(existing.node);
    let server = server.map(str::to_string).or(existing.server);

    let mut toml = String::new();
    toml.push_str("# LiteBin project config — used by `l8b deploy/env/status` as defaults.\n");
    toml.push_str("# Written by `l8b init`, updated automatically after a successful deploy.\n");
    toml.push_str("# Commit this file — it holds no secrets; env values live on the server (l8b env push).\n");
    toml.push_str(&format!("project = \"{project}\"\n"));
    if let Some(ref n) = node {
        toml.push_str(&format!("node = \"{n}\"\n"));
    }
    if let Some(ref s) = server {
        toml.push_str(&format!("server = \"{s}\"\n"));
    }

    let path = dir.join(FILE_NAME);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(toml.as_str()) {
        return Ok(false);
    }
    std::fs::write(&path, &toml)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_deploy_writes_and_merges() {
        let dir = std::env::temp_dir().join(format!("l8b-toml-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        // Fresh deploy: file created with the project.
        assert!(record_deploy(&dir, "myapp", None, None).unwrap());
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.project.as_deref(), Some("myapp"));
        assert_eq!(cfg.node, None);

        // Redeploy with a node: node added.
        assert!(record_deploy(&dir, "myapp", Some("worker-1"), None).unwrap());
        assert_eq!(load(&dir).unwrap().node.as_deref(), Some("worker-1"));

        // A server binding sticks and survives later redeploys that omit it.
        assert!(record_deploy(&dir, "myapp", Some("worker-1"), Some("https://s.example")).unwrap());
        assert_eq!(load(&dir).unwrap().server.as_deref(), Some("https://s.example"));
        assert!(!record_deploy(&dir, "myapp", None, None).unwrap());
        assert_eq!(load(&dir).unwrap().server.as_deref(), Some("https://s.example"));

        // The rewrite keeps only project, node, and server.
        std::fs::write(
            dir.join(FILE_NAME),
            "project = \"myapp\"\nport = 3000\nnode = \"worker-1\"\nenv_file = \".env\"\n",
        )
        .unwrap();
        assert!(record_deploy(&dir, "myapp", None, None).unwrap());
        let raw = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(!raw.contains("env_file"));
        assert!(!raw.contains("port"));
        assert_eq!(load(&dir).unwrap().node.as_deref(), Some("worker-1"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
