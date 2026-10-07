//! Build-time env resolution for Railpack builds: selected .env files →
//! (NAME, VALUE) pairs passed via process env + name-only `--env` flags.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use colored::Colorize;

/// Railpack's `--env` parser only accepts `[A-Za-z0-9_+-]` names; anything
/// else is silently mangled (e.g. `A.B=1` becomes name `A`, value `.B`).
fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-')
}

/// Parse env files (in precedence order, later files win) into a sorted map.
/// A missing file is a hard error so an explicit `--secret` typo is not
/// silently ignored. Returns the map plus names skipped as invalid.
pub(super) fn parse_env_files(files: &[PathBuf]) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut vars = BTreeMap::new();
    let mut skipped = Vec::new();
    for file in files {
        let raw = std::fs::read(file).with_context(|| format!("failed to read env file {}", file.display()))?;
        for item in dotenvy::Iter::new(raw.as_slice()) {
            let Ok((key, value)) = item else { continue };
            let key = key.trim().to_string();
            if valid_env_name(&key) {
                vars.insert(key, value);
            } else {
                skipped.push(key);
            }
        }
    }
    Ok((vars, skipped))
}

/// external-then-context with exact-path duplicates dropped (first wins,
/// preserving root-before-service precedence).
pub(super) fn dedup_env_files(external: &[PathBuf], context: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    external.iter().chain(context).filter(|p| seen.insert((*p).clone())).cloned().collect()
}

/// Env pairs for a Railpack build from external (root) and context env files.
pub(super) fn railpack_env_pairs(
    external: &[PathBuf],
    context: &[PathBuf],
) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    parse_env_files(&dedup_env_files(external, context))
}

/// Warn (non-CI) about env names skipped because Railpack cannot take them.
pub(super) fn warn_skipped(names: &[String], ci_mode: bool) {
    if !names.is_empty() && !ci_mode {
        println!(
            "  {} Skipping vars Railpack cannot accept (invalid names): {}",
            "!".yellow(),
            names.join(", ").dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("l8b-secrets-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_last_one_wins_and_skips_comments() {
        let dir = temp_dir("parse");
        let a = write(&dir, "a", "# comment\nFOO=1\nBAR=hello\n");
        let b = write(&dir, "b", "FOO=2\nQ=\"a b\"\n");

        let (vars, skipped) = parse_env_files(&[a, b]).unwrap();
        assert_eq!(vars.get("FOO").unwrap(), "2");
        assert_eq!(vars.get("BAR").unwrap(), "hello");
        assert_eq!(vars.get("Q").unwrap(), "a b");
        assert!(skipped.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_names_reported() {
        let dir = temp_dir("invalid");
        let a = write(&dir, "a", "A.B=1\nOK=yes\n");

        let (vars, skipped) = parse_env_files(&[a]).unwrap();
        assert_eq!(vars.get("OK").unwrap(), "yes");
        assert_eq!(skipped, vec!["A.B".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_error() {
        let dir = temp_dir("missing");
        let err = parse_env_files(&[dir.join("nope.env")]).unwrap_err();
        assert!(err.to_string().contains("failed to read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_keeps_first_occurrence() {
        let root = PathBuf::from("/proj/.env");
        let svc = PathBuf::from("/proj/web/.env");
        let merged = dedup_env_files(std::slice::from_ref(&root), &[svc.clone(), root.clone()]);
        assert_eq!(merged, vec![root, svc]);
    }
}
