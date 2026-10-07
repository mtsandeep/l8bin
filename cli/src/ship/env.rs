use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;
use dialoguer::{MultiSelect, Select};

pub(super) fn env_precedence_score(name: &str) -> i32 {
    if name == ".env.example" {
        0
    } else if name == ".env" {
        1
    } else if name.contains(".local") || name.contains(".prod") {
        3
    } else {
        2
    }
}

pub(super) fn discover_env_files(dir: &Path, exclude_example: bool) -> Result<Vec<String>> {
    let mut env_files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".env"))
        .filter(|name| !(exclude_example && name == ".env.example"))
        .collect();

    env_files.sort_by(|a, b| env_precedence_score(a).cmp(&env_precedence_score(b)).then(a.cmp(b)));
    Ok(env_files)
}

/// How build-time env files were chosen from flags.
#[derive(Clone)]
pub(crate) enum EnvSource {
    /// All .env* files except .env.example, precedence order.
    Auto,
    /// `--secret` list, used verbatim.
    Explicit(Vec<PathBuf>),
    /// `--no-env`
    Disabled,
}

impl EnvSource {
    pub(crate) fn from_flags(secret: Vec<PathBuf>, no_env: bool) -> Self {
        if !secret.is_empty() {
            Self::Explicit(secret)
        } else if no_env {
            Self::Disabled
        } else {
            Self::Auto
        }
    }
}

/// Resolve the env file list for a build. Interactive callers (ship) confirm
/// or adjust the auto-discovered set; non-interactive callers include all
/// discovered files, printing the list unless `quiet`.
pub(crate) fn resolve_env_files(
    project_dir: &Path,
    source: &EnvSource,
    interactive: bool,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    match source {
        EnvSource::Explicit(files) => Ok(files.clone()),
        EnvSource::Disabled => Ok(Vec::new()),
        EnvSource::Auto => {
            let env_files = discover_env_files(project_dir, true)?;
            if env_files.is_empty() {
                return Ok(Vec::new());
            }
            if !interactive {
                if !quiet {
                    println!("  {} Using .env files: {}", "::".dimmed(), env_files.join(", ").dimmed());
                }
                return Ok(env_files.into_iter().map(|n| project_dir.join(n)).collect());
            }

            println!("  {} Found .env files: {}", "::".dimmed(), env_files.join(", ").dimmed());
            loop {
                let choices = ["Use these files", "Pick specific files...", "Skip build env"];
                let selection =
                    Select::new().with_prompt("  🔒 Build-time env").items(&choices).default(0).interact()?;

                match selection {
                    0 => return Ok(env_files.iter().map(|n| project_dir.join(n)).collect()),
                    1 => {
                        let chosen = MultiSelect::new()
                            .with_prompt("  🔒 Select files [Space to select, Enter to confirm]")
                            .items(&env_files)
                            .interact()?;

                        if chosen.is_empty() {
                            println!(
                                "  {} {}",
                                "!".red(),
                                "No files selected. Pick at least one, or choose 'Skip build env'.".yellow()
                            );
                            continue;
                        }
                        let selected: Vec<&str> = chosen.iter().map(|&i| env_files[i].as_str()).collect();
                        println!("  {} Including: {}", "::".dimmed(), selected.join(", ").dimmed());
                        return Ok(chosen.into_iter().map(|i| project_dir.join(&env_files[i])).collect());
                    }
                    _ => {
                        println!("  {} No build-time env included", "::".dimmed());
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }
}

/// .env* files directly inside `svc_dir` (precedence order, .env.example excluded).
pub(super) fn service_env_files(svc_dir: &Path) -> Vec<PathBuf> {
    discover_env_files(svc_dir, true)
        .map(|files| files.into_iter().map(|n| svc_dir.join(n)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("l8b-shipenv-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discover_orders_by_precedence_and_excludes_example() {
        let dir = temp_dir("discover");
        for name in [".env", ".env.example", ".env.local", ".env.prod", ".env.staging"] {
            std::fs::write(dir.join(name), "A=1\n").unwrap();
        }

        let found = discover_env_files(&dir, true).unwrap();
        assert_eq!(found, vec![".env", ".env.staging", ".env.local", ".env.prod"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_flags_priority() {
        assert!(matches!(EnvSource::from_flags(vec![PathBuf::from(".env")], true), EnvSource::Explicit(_)));
        assert!(matches!(EnvSource::from_flags(vec![], true), EnvSource::Disabled));
        assert!(matches!(EnvSource::from_flags(vec![], false), EnvSource::Auto));
    }

    #[test]
    fn resolve_explicit_and_disabled_without_prompt() {
        let dir = temp_dir("resolve");
        std::fs::write(dir.join(".env"), "A=1\n").unwrap();

        let explicit = resolve_env_files(&dir, &EnvSource::Explicit(vec![dir.join(".env")]), false, false).unwrap();
        assert_eq!(explicit, vec![dir.join(".env")]);

        let disabled = resolve_env_files(&dir, &EnvSource::Disabled, false, false).unwrap();
        assert!(disabled.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
