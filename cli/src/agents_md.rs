//! `AGENTS.md` — repo-level instructions for coding agents. `l8b init
//! --agents` merges a managed `## Deploying (LiteBin)` section into it:
//! created when absent, appended to user content, refreshed in place.
//! User content is never removed.

use anyhow::Result;
use std::path::Path;

pub const FILE_NAME: &str = "AGENTS.md";
const SECTION_TITLE: &str = "Deploying (LiteBin)";

fn section_body() -> &'static str {
    "\
LiteBin is the platform; `l8bin` is pronounced \"LiteBin\" and `l8b` is its CLI (same project: repo github.com/mtsandeep/l8bin, site l8bin.com). Full agent docs: https://l8bin.com/llms.txt

This project deploys to LiteBin (`server` in `l8b.toml`). The MCP server is
wired in `.mcp.json`; prefer the LiteBin MCP tools, or fall back to the CLI.

- Deploy the current directory: tool `deploy` (or `l8b deploy`). After a
  successful deploy, `l8b.toml` records the project so future sessions need
  no arguments.
- Check if it's up: tool `status` with `wait: true, healthy: true` (or
  `l8b status --wait --healthy`); succeeds only when serving HTTP 200. The
  reported URL is the custom domain when one is set.
- Read logs on failure: tool `logs` (or `l8b logs`).
- Runtime secrets (DATABASE_URL etc.): tool `env_push`, then `restart`.
  Values are write-only; never expect to read them back.
- Full command list: `l8b --help`; machine output: add `--json`.

If auth fails, call the `setup` MCP tool with no arguments and follow its
`state` + `next` (it pairs auth and binds the workspace). Without the MCP,
run `l8b login --server <url> --pair` and relay the printed approval URL to
the user; the command returns once they approve. Never ask for passwords.
"
}

/// Merge the managed section into `dir/AGENTS.md`. Returns true when the
/// file changed. Idempotent: a second run rewrites identical content.
pub fn merge_section(dir: &Path) -> Result<bool> {
    let path = dir.join(FILE_NAME);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = splice(&existing);
    if updated == existing {
        return Ok(false);
    }
    std::fs::write(&path, updated)?;
    Ok(true)
}

/// (heading_start, body_end) of the managed section, if present. The body
/// runs to the next `\n## ` heading or EOF.
fn find_section(existing: &str) -> Option<(usize, usize)> {
    let title = format!("## {SECTION_TITLE}");
    let mut search = 0;
    while let Some(rel) = existing[search..].find(&title) {
        let start = search + rel;
        let at_line_start = start == 0 || existing[..start].ends_with('\n');
        // "## Title" must not be a longer heading like "## Title (old)".
        let line_end = existing[start..].find('\n').map(|o| start + o).unwrap_or(existing.len());
        let is_exact = existing[start..line_end].trim_end() == title;
        if at_line_start && is_exact {
            let end = existing[line_end..].find("\n## ").map(|o| line_end + o + 1).unwrap_or(existing.len());
            return Some((start, end));
        }
        search = start + 1;
    }
    None
}

fn splice(existing: &str) -> String {
    let (before, after) = match find_section(existing) {
        Some((start, end)) => {
            (existing[..start].trim_end().to_string(), existing[end..].trim_start_matches('\n').to_string())
        }
        None => (existing.trim_end().to_string(), String::new()),
    };

    let mut out = String::new();
    if before.is_empty() {
        out.push_str("# AGENTS.md\n\n");
    } else {
        out.push_str(&before);
        out.push_str("\n\n");
    }
    out.push_str(&format!("## {SECTION_TITLE}\n\n"));
    out.push_str(section_body());
    if !after.is_empty() {
        out.push('\n');
        out.push_str(&after);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_fresh_file() {
        let dir = std::env::temp_dir().join(format!("l8b-agents-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        assert!(merge_section(&dir).unwrap());
        let raw = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(raw.starts_with("# AGENTS.md\n\n## Deploying (LiteBin)"));
        assert!(raw.contains("env_push"));
        // Idempotent.
        assert!(!merge_section(&dir).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn appends_after_user_content() {
        let existing = "# My rules\n\n- be nice\n";
        let out = splice(existing);
        assert!(out.starts_with("# My rules\n\n- be nice\n\n## Deploying (LiteBin)"));
        assert!(out.ends_with("Never ask for passwords.\n"));
    }

    #[test]
    fn refreshes_in_place_without_touching_neighbors() {
        let existing = "# T\n\n## Deploying (LiteBin)\n\nstale content\n\n## Other\n\nkeep me\n";
        let out = splice(existing);
        assert!(!out.contains("stale content"));
        assert!(out.contains("env_push"));
        assert!(out.contains("## Other\n\nkeep me"));
        // Section sits before the user's next heading.
        assert!(out.find("env_push").unwrap() < out.find("## Other").unwrap());
        // Idempotent through the file-level API shape.
        assert_eq!(splice(&out), out);
    }

    #[test]
    fn ignores_lookalike_headings() {
        let existing = "## Deploying (LiteBin) (legacy)\n\nkeep\n";
        let out = splice(existing);
        // Not an exact match: appended, not replaced.
        assert!(out.contains("keep"));
        assert!(out.matches("## Deploying (LiteBin)").count() == 2);
    }
}
