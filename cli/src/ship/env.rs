use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;
use dialoguer::{Input, MultiSelect, Select};

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

pub(super) enum EnvSelectMode {
    Interactive,
    InteractiveNoCustomOrder,
    AutoAllExceptExample,
}

pub(super) fn select_env_files(project_dir: &Path, mode: EnvSelectMode) -> Result<Vec<PathBuf>> {
    match mode {
        EnvSelectMode::AutoAllExceptExample => {
            let files = discover_env_files(project_dir, true)?;
            if !files.is_empty() {
                println!("  {} Using .env files: {}", "::".dimmed(), files.join(", ").dimmed());
            }
            Ok(files.into_iter().map(|n| project_dir.join(n)).collect())
        }
        EnvSelectMode::Interactive | EnvSelectMode::InteractiveNoCustomOrder => {
            let env_files = discover_env_files(project_dir, false)?;
            if env_files.is_empty() {
                return Ok(Vec::new());
            }

            let allow_custom = matches!(mode, EnvSelectMode::Interactive);
            loop {
                let mut choices =
                    vec!["Yes (all / standard order)".to_string(), "No".to_string(), "Pick specific...".to_string()];
                if allow_custom {
                    choices.push("Custom order (manual input)".to_string());
                }

                let selection = Select::new()
                    .with_prompt("  🔒 Found .env files. Include build-time secrets?")
                    .items(&choices)
                    .default(0)
                    .interact()?;

                match selection {
                    0 => {
                        println!("  {} Using standard merge order (later files override earlier ones):", "::".dimmed());
                        println!("     {}", env_files.join(" < ").dimmed());
                        return Ok(env_files.iter().map(|n| project_dir.join(n)).collect());
                    }
                    1 => {
                        println!("  {} No build-time secrets included", "::".dimmed());
                        return Ok(Vec::new());
                    }
                    2 => {
                        let chosen = MultiSelect::new()
                            .with_prompt(
                                "  🔒 Select secrets (Standard merge order applies) [Space to select, Enter to confirm]",
                            )
                            .items(&env_files)
                            .interact()?;

                        if chosen.is_empty() {
                            println!(
                                "  {} {}",
                                "!".red(),
                                "No files selected. Pick at least one, or choose 'No' to continue without secrets."
                                    .yellow()
                            );
                            continue;
                        }

                        let selected: Vec<&str> = chosen.iter().map(|&i| env_files[i].as_str()).collect();
                        println!("  {} Merging: {}", "::".dimmed(), selected.join(" < ").dimmed());
                        return Ok(chosen.into_iter().map(|i| project_dir.join(&env_files[i])).collect());
                    }
                    3 if allow_custom => {
                        println!("  {} Available files: {}", "::".dimmed(), env_files.join(", ").dimmed());
                        let input: String = Input::new()
                            .with_prompt("  🔒 Enter filenames in merge order (space separated, e.g. .env .env.local)")
                            .interact_text()?;

                        let parts: Vec<&str> = input.split_whitespace().collect();
                        let mut selected = Vec::new();
                        for part in &parts {
                            let path = project_dir.join(part);
                            if path.exists() {
                                selected.push(path);
                            } else {
                                println!("  {} {} does not exist, skipping", "!".yellow(), part);
                            }
                        }
                        if selected.is_empty() {
                            println!("  {} {}", "!".red(), "No valid files entered.".yellow());
                            continue;
                        }
                        println!("  {} Merging in your exact order: {}", "::".dimmed(), parts.join(" < ").dimmed());
                        return Ok(selected);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

pub(super) fn merge_service_env_files(root_env: &[PathBuf], svc_dir: &Path) -> Vec<PathBuf> {
    let mut secret = root_env.to_vec();
    if let Ok(entries) = std::fs::read_dir(svc_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".env") && !secret.iter().any(|p| p.file_name() == Some(entry.file_name().as_os_str()))
            {
                secret.push(entry.path());
            }
        }
    }
    secret
}
