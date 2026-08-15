use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};

/// A guard that manages the temporary build context (.env and .dockerignore)
pub struct BuildContextGuard {
    _project_dir: PathBuf,
    env_path: PathBuf,
    env_backup: Option<PathBuf>,
    pub ignore_path: Option<PathBuf>,
}

impl BuildContextGuard {
    pub fn new(project_dir: &Path, dockerfile_name: &str, secret_files: Vec<PathBuf>) -> Result<Self> {
        let has_secrets = !secret_files.is_empty();

        // 1. Merge secrets into .env (only when secrets are provided)
        let env_path = project_dir.join(".env");
        let mut env_backup = None;

        if has_secrets {
            let mut merged_env = std::collections::BTreeMap::new();
            for file in secret_files {
                if let Ok(content) = std::fs::read_to_string(&file) {
                    for line in content.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        if let Some((key, val)) = line.split_once('=') {
                            merged_env.insert(key.trim().to_string(), val.trim().to_string());
                        }
                    }
                }
            }

            let merged_content = merged_env.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("\n");

            // Backup existing .env if present
            if env_path.exists() {
                let backup_path = project_dir.join(format!(
                    ".env.l8b_backup_{}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs()
                ));
                std::fs::rename(&env_path, &backup_path)?;
                env_backup = Some(backup_path);
            }
            std::fs::write(&env_path, merged_content)?;
        }

        // 2. Generate <Dockerfile>.dockerignore via naming convention (No Touch strategy)
        // Docker automatically looks for [Dockerfile-name].dockerignore
        let mut ignore_content = String::new();

        // Load existing ignore patterns
        let existing_ignore = project_dir.join(".dockerignore");
        let gitignore = project_dir.join(".gitignore");

        let mut base_ignore = String::new();
        if existing_ignore.exists() {
            base_ignore = std::fs::read_to_string(&existing_ignore)?;
        } else if gitignore.exists() {
            base_ignore = std::fs::read_to_string(&gitignore)?;
        }

        ignore_content.push_str(&base_ignore);
        // Only force-include .env when we injected secrets (otherwise respect the ignore patterns)
        if has_secrets {
            ignore_content.push_str("\n!.env*\n");
        }

        let dockerfile_basename = Path::new(dockerfile_name).file_name().unwrap_or_default().to_string_lossy();
        let target_ignore = project_dir.join(format!("{}.dockerignore", dockerfile_basename));

        // Only write if there's actual content or we need the !.env* override
        if !ignore_content.trim().is_empty() || has_secrets {
            std::fs::write(&target_ignore, &ignore_content)?;
        }

        Ok(Self {
            _project_dir: project_dir.to_path_buf(),
            env_path,
            env_backup,
            ignore_path: if !ignore_content.trim().is_empty() || has_secrets { Some(target_ignore) } else { None },
        })
    }
}

impl Drop for BuildContextGuard {
    fn drop(&mut self) {
        // Cleanup injected .env
        let _ = std::fs::remove_file(&self.env_path);
        if let Some(ref backup) = self.env_backup {
            let _ = std::fs::rename(backup, &self.env_path);
        }

        // Cleanup temporary ignore file
        if let Some(ref ip) = self.ignore_path {
            let _ = std::fs::remove_file(ip);
        }
    }
}

/// Clean up leftover build artifacts from interrupted builds.
///
/// Scans for `.env.l8b_backup_*` files and `<Dockerfile>.dockerignore` temp files,
/// prompts the user before restoring .env if both files have content.
pub fn cleanup_build_artifacts(dir: &Path) -> Result<()> {
    let mut cleaned = 0usize;
    let mut skipped = 0usize;

    // 1. Find and restore .env backups
    let backups: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".env.l8b_backup_"))
        .collect();

    if !backups.is_empty() {
        println!("  Found {} backup file(s):", backups.len());

        for entry in &backups {
            let backup_path = entry.path();
            let env_path = dir.join(".env");
            let name = backup_path.file_name().unwrap_or_default().to_string_lossy();

            let backup_content = std::fs::read_to_string(&backup_path).unwrap_or_default();
            let backup_has_values = backup_content.lines().any(|l| {
                let l = l.trim();
                !l.is_empty() && !l.starts_with('#') && l.contains('=')
            });

            let env_exists = env_path.exists();
            let env_has_values = if env_exists {
                let env_content = std::fs::read_to_string(&env_path).unwrap_or_default();
                env_content.lines().any(|l| {
                    let l = l.trim();
                    !l.is_empty() && !l.starts_with('#') && l.contains('=')
                })
            } else {
                false
            };

            let should_restore = if env_exists && env_has_values && !backup_has_values {
                // Backup is empty but current .env has values — risky
                println!("    {} {} — backup is empty but current .env has content", "!".yellow(), name);
                println!("       Skipping to avoid data loss.");
                false
            } else if env_exists && env_has_values && backup_has_values {
                // Both have values — confirm
                println!("    {} {} — both backup and current .env have content", "!".yellow(), name);

                dialoguer::Confirm::new()
                    .with_prompt("       Replace current .env with backup?")
                    .default(false)
                    .interact()?
            } else {
                // Safe to restore (no current .env, or current is empty/placeholder)
                true
            };

            if should_restore {
                if env_exists {
                    let _ = std::fs::remove_file(&env_path);
                }
                match std::fs::rename(&backup_path, &env_path) {
                    Ok(()) => {
                        println!("    {} Restored {} -> .env", "✔".green(), name);
                        cleaned += 1;
                    }
                    Err(e) => {
                        println!("    {} Failed to restore {}: {}", "✗".red(), name, e);
                        skipped += 1;
                    }
                }
            } else {
                skipped += 1;
            }
        }
    }

    // 2. Find and remove temp dockerignore files
    let dockerignores: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            name_str.ends_with(".dockerignore") && name_str != ".dockerignore"
        })
        .collect();

    for entry in &dockerignores {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("    {} Removed {}", "✔".green(), name);
                cleaned += 1;
            }
            Err(e) => {
                println!("    {} Failed to remove {}: {}", "✗".red(), name, e);
                skipped += 1;
            }
        }
    }

    if cleaned == 0 && skipped == 0 {
        println!("  No build artifacts found. Directory is clean.");
    } else {
        println!();
        println!("  Cleaned: {}, Skipped: {}", cleaned.to_string().green(), skipped);
    }

    Ok(())
}

/// Return directory names to mask in the Docker build context on Windows.
/// Reads `.dockerignore` if present, otherwise falls back to `.gitignore`.
/// Only returns top-level directory names that actually exist on disk.
pub(crate) fn gitignored_dirs(project_dir: &Path) -> Vec<String> {
    let mut dirs = std::collections::HashSet::new();

    // Always try to mask these common suspects if they exist
    let defaults = vec![".git", "node_modules", ".next", ".vercel", "build", "dist", "target"];
    for d in defaults {
        if project_dir.join(d).is_dir() {
            dirs.insert(d.to_string());
        }
    }

    let ignore_file = if project_dir.join(".dockerignore").exists() {
        project_dir.join(".dockerignore")
    } else {
        project_dir.join(".gitignore")
    };

    if let Ok(content) = std::fs::read_to_string(&ignore_file) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }

            // Clean the pattern
            let pattern = line.trim_start_matches('/');

            // On Windows, we can only reliably mask top-level directories via --tmpfs.
            // If the pattern is complex (has * or / in the middle), we skip it
            // and let the internal .dockerignore handle it.

            // 1. If it's a simple directory name (e.g. "node_modules" or ".next/")
            let clean_pattern = pattern.trim_end_matches('/');
            if !clean_pattern.contains('/') && !clean_pattern.contains('*') && project_dir.join(clean_pattern).is_dir()
            {
                dirs.insert(clean_pattern.to_string());
            }
        }
    }

    dirs.into_iter().collect()
}
