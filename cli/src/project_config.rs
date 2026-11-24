//! `l8b.toml` / `l8b.local.toml` — per-repo deploy defaults (no secrets; env
//! values live server-side). Same format both files; local wins collisions.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE_NAME: &str = "l8b.toml";
pub const LOCAL_FILE_NAME: &str = "l8b.local.toml";

/// One deploy target: a server plus the project (and optional node) on it.
#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq)]
pub struct Destination {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ProjectConfig {
    pub project: Option<String>,
    pub node: Option<String>,
    pub server: Option<String>,
    /// Named destination `ship`/`deploy` target; the pointer lives locally.
    pub active: Option<String>,
    /// Which compose file(s) this repo deploys (a variant, or several merged
    /// in order); local wins.
    #[serde(default, deserialize_with = "deserialize_compose")]
    pub compose: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub destinations: BTreeMap<String, Destination>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Committed,
    Local,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Committed => "l8b.toml",
            Origin::Local => "local",
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Merged {
    pub project: Option<String>,
    #[allow(dead_code)]
    pub node: Option<String>,
    pub server: Option<String>,
    pub active: Option<String>,
    pub compose: Option<Vec<String>>,
    pub destinations: BTreeMap<String, (Destination, Origin)>,
}

impl Merged {
    /// The active destination, when the pointer names one that exists.
    pub fn active_destination(&self) -> Option<(&String, &Destination)> {
        let name = self.active.as_ref()?;
        self.destinations.get(name).map(|(d, _)| (name, d))
    }
}

pub fn load(dir: &Path) -> Option<ProjectConfig> {
    let raw = std::fs::read_to_string(dir.join(FILE_NAME)).ok()?;
    parse(&raw, FILE_NAME)
}

pub fn load_local(dir: &Path) -> Option<ProjectConfig> {
    let raw = std::fs::read_to_string(dir.join(LOCAL_FILE_NAME)).ok()?;
    parse(&raw, LOCAL_FILE_NAME)
}

/// `compose` accepts a single string or an array (merged in order).
fn deserialize_compose<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Compose {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<Compose>::deserialize(deserializer)? {
        Some(Compose::One(f)) => Some(vec![f]),
        Some(Compose::Many(v)) => Some(v),
        None => None,
    })
}

fn parse(raw: &str, file: &str) -> Option<ProjectConfig> {
    match toml::from_str(raw) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("warning: invalid {file}: {e}");
            None
        }
    }
}

/// Both files merged; local wins.
pub fn load_merged(dir: &Path) -> Merged {
    let base = load(dir).unwrap_or_default();
    let local = load_local(dir).unwrap_or_default();
    let mut destinations = BTreeMap::new();
    for (name, dest) in base.destinations {
        destinations.insert(name, (dest, Origin::Committed));
    }
    for (name, dest) in local.destinations {
        destinations.insert(name, (dest, Origin::Local));
    }
    Merged {
        project: local.project.or(base.project),
        node: local.node.or(base.node),
        server: local.server.or(base.server),
        active: local.active.or(base.active),
        compose: local.compose.or(base.compose),
        destinations,
    }
}

/// Explicit flag wins; then the active destination's project; then the anchor.
pub fn resolve_project(explicit: Option<&str>, dir: &Path) -> Result<String> {
    if let Some(p) = explicit.filter(|s| !s.is_empty()) {
        return Ok(p.to_string());
    }
    let merged = load_merged(dir);
    if let Some((_, dest)) = merged.active_destination()
        && let Some(p) = &dest.project
    {
        return Ok(p.clone());
    }
    if let Some(p) = merged.project {
        return Ok(p);
    }
    bail!("no project specified — pass it explicitly or set `project` in {FILE_NAME} (run `l8b init`)");
}

/// The server non-interactive commands target: the active destination's
/// server, else the anchor `server`.
pub fn resolve_server(dir: &Path) -> Option<String> {
    let merged = load_merged(dir);
    merged.active_destination().map(|(_, d)| d.server.clone()).or(merged.server)
}

/// The project `ship` defaults to: active destination's, else the anchor.
pub fn resolve_stored_project(dir: &Path) -> Option<String> {
    let merged = load_merged(dir);
    merged.active_destination().and_then(|(_, d)| d.project.clone()).or(merged.project)
}

/// Name suggestion from a server URL: "http://localhost:5080" → "localhost-5080".
pub fn destination_name_from_url(server: &str) -> String {
    let stripped = server.trim_start_matches("https://").trim_start_matches("http://");
    let cleaned: String =
        stripped.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' }).collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() { "dest".to_string() } else { trimmed }
}

/// Record a successful deploy: the matching destination's entry is updated in
/// its own file; without destinations the anchor is rewritten as before.
pub fn record_deploy(dir: &Path, project: &str, node: Option<&str>, server: Option<&str>) -> Result<bool> {
    let merged = load_merged(dir);
    let server = server.filter(|s| !s.is_empty());

    let target =
        if let Some((name, dest)) = merged.active_destination().filter(|(_, d)| Some(d.server.as_str()) == server) {
            Some((name.clone(), dest.clone(), Origin::Local))
        } else {
            merged
                .destinations
                .iter()
                .find(|(_, (d, _))| Some(d.server.as_str()) == server)
                .map(|(name, (d, o))| (name.clone(), d.clone(), *o))
        };
    if let Some((name, mut dest, origin)) = target {
        dest.project = Some(project.to_string());
        if node.is_some() {
            dest.node = node.map(str::to_string);
        }
        return match origin {
            Origin::Local => {
                let mut cfg = read_or_default(dir, LOCAL_FILE_NAME);
                cfg.destinations.insert(name.clone(), dest);
                cfg.active = Some(name);
                write_file(dir, LOCAL_FILE_NAME, &cfg)
            }
            Origin::Committed => {
                let mut cfg = read_or_default(dir, FILE_NAME);
                cfg.destinations.insert(name.clone(), dest);
                write_file(dir, FILE_NAME, &cfg)?;
                set_active(dir, &name)
            }
        };
    }
    if !merged.destinations.is_empty() {
        // Destinations exist but none matches this server — a one-off target;
        // leave both files alone rather than repoint anything.
        return Ok(false);
    }

    // No destinations: the anchor tracks the default target, but never
    // repoints to a different server (ship offers to save that instead).
    let mut cfg = load(dir).unwrap_or_default();
    if cfg.server.as_deref().is_some_and(|prev| Some(prev) != server) {
        return Ok(false);
    }
    cfg.project = Some(project.to_string());
    if node.is_some() {
        cfg.node = node.map(str::to_string);
    }
    if server.is_some() {
        cfg.server = server.map(str::to_string);
    }
    write_file(dir, FILE_NAME, &cfg)
}

/// Save a named destination to the local file and point `active` at it.
pub fn save_destination(dir: &Path, name: &str, dest: Destination) -> Result<bool> {
    let mut cfg = read_or_default(dir, LOCAL_FILE_NAME);
    let changed = cfg.destinations.get(name) != Some(&dest) || cfg.active.as_deref() != Some(name);
    cfg.destinations.insert(name.to_string(), dest);
    cfg.active = Some(name.to_string());
    Ok(write_file(dir, LOCAL_FILE_NAME, &cfg)? && changed)
}

/// Save a destination to the committed file; never touches `active`.
pub fn save_destination_committed(dir: &Path, name: &str, dest: Destination) -> Result<bool> {
    let mut cfg = read_or_default(dir, FILE_NAME);
    cfg.destinations.insert(name.to_string(), dest);
    write_file(dir, FILE_NAME, &cfg)
}

/// Point `active` at an existing destination (always in the local file).
pub fn set_active(dir: &Path, name: &str) -> Result<bool> {
    let mut cfg = read_or_default(dir, LOCAL_FILE_NAME);
    if cfg.active.as_deref() == Some(name) {
        return Ok(false);
    }
    cfg.active = Some(name.to_string());
    write_file(dir, LOCAL_FILE_NAME, &cfg)
}

/// Remove a destination; clears `active` when it pointed there.
/// Remember the compose file choice in the local file.
pub fn set_compose(dir: &Path, name: &str) -> Result<bool> {
    set_compose_list(dir, vec![name.to_string()])
}

/// Remember a multi-file (merged) compose choice in the local file.
pub fn set_compose_list(dir: &Path, names: Vec<String>) -> Result<bool> {
    let mut cfg = read_or_default(dir, LOCAL_FILE_NAME);
    if cfg.compose.as_deref() == Some(names.as_slice()) {
        return Ok(false);
    }
    cfg.compose = Some(names);
    write_file(dir, LOCAL_FILE_NAME, &cfg)
}

pub fn remove_destination(dir: &Path, name: &str) -> Result<()> {
    let merged = load_merged(dir);
    let Some((_, origin)) = merged.destinations.get(name) else {
        bail!("no destination named '{name}'");
    };
    let file = if *origin == Origin::Local { LOCAL_FILE_NAME } else { FILE_NAME };
    let mut cfg = read_or_default(dir, file);
    cfg.destinations.remove(name);
    if cfg.active.as_deref() == Some(name) {
        cfg.active = None;
    }
    write_file(dir, file, &cfg)?;
    if *origin == Origin::Committed {
        let mut local = read_or_default(dir, LOCAL_FILE_NAME);
        if local.active.as_deref() == Some(name) {
            local.active = None;
            write_file(dir, LOCAL_FILE_NAME, &local)?;
        }
    }
    Ok(())
}

fn read_or_default(dir: &Path, file: &str) -> ProjectConfig {
    match file {
        LOCAL_FILE_NAME => load_local(dir).unwrap_or_default(),
        _ => load(dir).unwrap_or_default(),
    }
}

pub(crate) fn render(file: &str, cfg: &ProjectConfig) -> String {
    let mut out = String::new();
    if file == LOCAL_FILE_NAME {
        out.push_str("# Local LiteBin destinations — not for commit; values here win over l8b.toml.\n");
    } else {
        out.push_str("# LiteBin deploy defaults — written by `l8b init` and deploys; no secrets here.\n");
        out.push_str("# Commit only if teammates/agents should inherit this target.\n");
    }
    if let Some(p) = &cfg.project {
        out.push_str(&format!("project = \"{p}\"\n"));
    }
    if let Some(n) = &cfg.node {
        out.push_str(&format!("node = \"{n}\"\n"));
    }
    if let Some(s) = &cfg.server {
        out.push_str(&format!("server = \"{s}\"\n"));
    }
    if let Some(a) = &cfg.active {
        out.push_str(&format!("active = \"{a}\"\n"));
    }
    if let Some(files) = &cfg.compose {
        match files.len() {
            0 => {}
            1 => out.push_str(&format!(
                "compose = \"{}\"
",
                files[0]
            )),
            _ => out.push_str(&format!(
                "compose = [{}]
",
                files.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ")
            )),
        }
    }
    for (name, dest) in &cfg.destinations {
        out.push_str(&format!("\n[destinations.{name}]\nserver = \"{}\"\n", dest.server));
        if let Some(p) = &dest.project {
            out.push_str(&format!("project = \"{p}\"\n"));
        }
        if let Some(n) = &dest.node {
            out.push_str(&format!("node = \"{n}\"\n"));
        }
    }
    out
}

fn write_file(dir: &Path, file: &str, cfg: &ProjectConfig) -> Result<bool> {
    let toml = render(file, cfg);
    let path = dir.join(file);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(toml.as_str()) {
        return Ok(false);
    }
    std::fs::write(&path, toml)?;
    Ok(true)
}

/// Write/replace `server` in `dir/l8b.toml`, preserving the rest; creates the
/// file when absent. Returns true when the file changed.
pub fn set_server(dir: &Path, server: &str) -> Result<bool> {
    let mut cfg = load(dir).unwrap_or_default();
    if cfg.server.as_deref() == Some(server) {
        return Ok(false);
    }
    cfg.server = Some(server.to_string());
    write_file(dir, FILE_NAME, &cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("l8b-dest-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dest(server: &str, project: Option<&str>) -> Destination {
        Destination { server: server.to_string(), project: project.map(str::to_string), node: None }
    }

    #[test]
    fn local_overrides_committed_keys_and_names() {
        let dir = temp_dir("merge");
        std::fs::write(
            dir.join(FILE_NAME),
            "project = \"shared\"\nserver = \"https://team.io\"\n\n[destinations.staging]\nserver = \"https://stg.io\"\nproject = \"app-stg\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(LOCAL_FILE_NAME),
            "project = \"mine\"\n\n[destinations.staging]\nserver = \"https://my-stg.io\"\n",
        )
        .unwrap();

        let m = load_merged(&dir);
        assert_eq!(m.project.as_deref(), Some("mine"));
        assert_eq!(m.server.as_deref(), Some("https://team.io"));
        assert_eq!(m.destinations["staging"].0.server, "https://my-stg.io");
        assert_eq!(m.destinations["staging"].1, Origin::Local);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_destination_drives_resolution() {
        let dir = temp_dir("resolve");
        std::fs::write(
            dir.join(FILE_NAME),
            "project = \"anchor\"\nserver = \"https://team.io\"\n\n[destinations.prod]\nserver = \"https://prod.io\"\nproject = \"app\"\n",
        )
        .unwrap();

        // No pointer yet: anchor wins.
        assert_eq!(resolve_project(None, &dir).unwrap(), "anchor");
        assert_eq!(resolve_server(&dir).as_deref(), Some("https://team.io"));

        // Pointer set locally: destination wins over the committed anchor.
        set_active(&dir, "prod").unwrap();
        assert_eq!(resolve_project(None, &dir).unwrap(), "app");
        assert_eq!(resolve_server(&dir).as_deref(), Some("https://prod.io"));

        // Explicit flag still beats everything.
        assert_eq!(resolve_project(Some("x"), &dir).unwrap(), "x");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_updates_matching_destination_in_place() {
        let dir = temp_dir("record");
        std::fs::write(
            dir.join(FILE_NAME),
            "project = \"anchor\"\nserver = \"https://team.io\"\n\n[destinations.prod]\nserver = \"https://prod.io\"\nproject = \"app\"\n",
        )
        .unwrap();
        save_destination(&dir, "sandbox", dest("http://localhost:5080", None)).unwrap();

        // Deploy to the local sandbox: its entry fills in, pointer moves.
        record_deploy(&dir, "myapp", None, Some("http://localhost:5080")).unwrap();
        let local = load_local(&dir).unwrap();
        assert_eq!(local.destinations["sandbox"].project.as_deref(), Some("myapp"));
        assert_eq!(local.active.as_deref(), Some("sandbox"));
        // Committed file untouched.
        assert!(load(&dir).unwrap().destinations["prod"].project.is_some());
        assert_eq!(load(&dir).unwrap().project.as_deref(), Some("anchor"));

        // Deploy to the committed prod destination: entry updates there,
        // pointer (still local) follows, committed anchor untouched.
        record_deploy(&dir, "app2", Some("n1"), Some("https://prod.io")).unwrap();
        assert_eq!(load(&dir).unwrap().destinations["prod"].project.as_deref(), Some("app2"));
        assert_eq!(load_local(&dir).unwrap().active.as_deref(), Some("prod"));

        // A server no destination claims: nothing is repointed.
        assert!(!record_deploy(&dir, "oneoff", None, Some("https://other.io")).unwrap());
        assert_eq!(load(&dir).unwrap().project.as_deref(), Some("anchor"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_rewrites_anchor_without_destinations() {
        let dir = temp_dir("anchor");
        record_deploy(&dir, "myapp", None, None).unwrap();
        assert_eq!(load(&dir).unwrap().project.as_deref(), Some("myapp"));

        record_deploy(&dir, "myapp", Some("worker-1"), Some("https://s.example")).unwrap();
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.node.as_deref(), Some("worker-1"));
        assert_eq!(cfg.server.as_deref(), Some("https://s.example"));

        // A deploy to a different server never repoints the anchor.
        assert!(!record_deploy(&dir, "oneoff", None, Some("https://other.io")).unwrap());
        assert_eq!(load(&dir).unwrap().server.as_deref(), Some("https://s.example"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_choice_merges_and_persists() {
        let dir = temp_dir("compose");
        std::fs::write(
            dir.join(FILE_NAME),
            "project = \"myapp\"
compose = \"docker-compose.prod.yml\"
",
        )
        .unwrap();

        // Committed choice is the default; a local choice overrides it.
        let prod = vec!["docker-compose.prod.yml".to_string()];
        let lite = vec!["docker-compose.staging.yml".to_string()];
        assert_eq!(load_merged(&dir).compose, Some(prod.clone()));
        set_compose(&dir, "docker-compose.staging.yml").unwrap();
        assert_eq!(load_merged(&dir).compose, Some(lite.clone()));
        assert_eq!(load(&dir).unwrap().compose, Some(prod));
        assert!(!set_compose(&dir, "docker-compose.staging.yml").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_clears_pointers() {
        let dir = temp_dir("remove");
        save_destination(&dir, "sbx", dest("http://localhost:5080", Some("app"))).unwrap();
        save_destination_committed(&dir, "prod", dest("https://prod.io", Some("app"))).unwrap();
        set_active(&dir, "prod").unwrap();

        remove_destination(&dir, "prod").unwrap();
        assert!(load_local(&dir).unwrap().active.is_none()); // pointer cleared
        assert!(load(&dir).unwrap().destinations.is_empty());

        remove_destination(&dir, "sbx").unwrap();
        assert!(load_local(&dir).unwrap().destinations.is_empty());
        assert!(remove_destination(&dir, "sbx").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
