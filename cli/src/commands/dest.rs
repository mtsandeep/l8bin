//! `l8b dest` — named deploy destinations in `l8b.toml` / `l8b.local.toml`.

use std::path::Path;

use anyhow::{Result, bail};
use colored::Colorize;
use dialoguer::Input;

use crate::config;
use crate::project_config::{self, Destination};

pub(crate) fn list(dir: &Path) -> Result<()> {
    let merged = project_config::load_merged(dir);
    if merged.destinations.is_empty() && merged.server.is_none() {
        println!("No destinations configured.");
        println!("  Add one: l8b dest add <name> --server <url>   (or just deploy somewhere with `l8b ship`)");
        return Ok(());
    }
    if let Some(server) = &merged.server {
        let project = merged.project.as_deref().unwrap_or("(set on first deploy)");
        println!("  anchor (default)  {}  {}", server.cyan(), project.dimmed());
    }
    for (name, (dest, origin)) in &merged.destinations {
        let marker = if merged.active.as_deref() == Some(name.as_str()) { "★" } else { " " };
        let project = dest.project.as_deref().unwrap_or("(created on first deploy)");
        let server = match &dest.node {
            Some(n) => format!("{} @{}", dest.server, n),
            None => dest.server.clone(),
        };
        println!(
            "  {marker} {:<16} {}  {}  [{}]  {}",
            name,
            server.cyan(),
            project.dimmed(),
            origin.label(),
            if marker == "★" { "active".green().to_string() } else { String::new() }
        );
    }
    Ok(())
}

pub(crate) struct AddArgs {
    pub name: Option<String>,
    pub server: Option<String>,
    pub project: Option<String>,
    pub node: Option<String>,
    pub commit: bool,
}

/// Add a destination; flags answer outright, the rest is prompted.
pub(crate) fn add(args: AddArgs, dir: &Path) -> Result<(String, Destination)> {
    let server: String = match args.server {
        Some(s) => s,
        None => Input::new().with_prompt("Server URL").interact_text()?,
    };
    if server.trim().is_empty() {
        bail!("server URL is required");
    }
    let name: String = match args.name {
        Some(n) => n,
        None => Input::new()
            .with_prompt("Destination name")
            .default(crate::project_config::destination_name_from_url(&server))
            .validate_with(|s: &String| {
                let ok = !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if ok { Ok(()) } else { Err("lowercase letters, digits, hyphens only") }
            })
            .interact_text()?,
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("destination name must use lowercase letters, digits and hyphens");
    }
    let project: Option<String> = match args.project {
        Some(p) => Some(p),
        None => Input::<String>::new()
            .with_prompt("Project ID (empty = created on first deploy)")
            .allow_empty(true)
            .default(String::new())
            .interact_text()?
            .into(),
    };
    let node = args.node;

    let dest = Destination { server: server.trim().to_string(), project, node };
    if args.commit {
        project_config::save_destination_committed(dir, &name, dest.clone())?;
        println!("  {} Saved '{}' → {} (committed file)", "✓".green(), name, project_config::FILE_NAME);
    } else {
        project_config::save_destination(dir, &name, dest.clone())?;
        println!(
            "  {} Saved '{}' → {} (local, not committed) and set active",
            "✓".green(),
            name,
            project_config::LOCAL_FILE_NAME
        );
        hint_gitignore(dir);
    }
    let paired = config::CredentialStore::load()
        .get(&config::normalize_server(&dest.server))
        .is_some_and(|c| c.auth_header().is_some());
    if !paired {
        println!("  {} Not paired with {} yet — run: l8b setup --server {}", "!".yellow(), dest.server, dest.server);
    }
    Ok((name, dest))
}

pub(crate) fn use_destination(name: &str, dir: &Path) -> Result<()> {
    let merged = project_config::load_merged(dir);
    if !merged.destinations.contains_key(name) {
        let names: Vec<String> = merged.destinations.keys().cloned().collect();
        bail!(
            "no destination named '{name}'{}",
            if names.is_empty() { String::new() } else { format!(" (have: {})", names.join(", ")) }
        );
    }
    project_config::set_active(dir, name)?;
    println!("  {} Active destination: {}", "✓".green(), name);
    Ok(())
}

pub(crate) fn remove(name: &str, dir: &Path) -> Result<()> {
    project_config::remove_destination(dir, name)?;
    println!("  {} Removed destination '{}'", "✓".green(), name);
    Ok(())
}

fn hint_gitignore(dir: &Path) {
    let gi = dir.join(".gitignore");
    if !project_config::load_local(dir).is_some_and(|_| true) {
        return;
    }
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == project_config::LOCAL_FILE_NAME) {
        return;
    }
    if gi.exists() {
        println!("  {} Add {} to .gitignore so it stays personal", "!".yellow(), project_config::LOCAL_FILE_NAME);
    } else {
        println!(
            "  {} No .gitignore here — add {} to it so the file stays personal",
            "!".yellow(),
            project_config::LOCAL_FILE_NAME
        );
    }
}
