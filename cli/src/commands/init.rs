//! `l8b init` — write `l8b.toml` (and optionally the workspace `.mcp.json`)
//! so coding agents working in this repo discover the LiteBin project.

use anyhow::{Result, bail};
use colored::Colorize;
use std::path::Path;

use crate::ci::CiMode;
use crate::out::Out;

pub(crate) struct InitArgs {
    pub project: Option<String>,
    pub node: Option<String>,
    pub server: Option<String>,
    pub mcp: bool,
    pub force: bool,
    pub path: std::path::PathBuf,
}

pub(crate) async fn run(args: InitArgs, ci_mode: &CiMode, out: &Out) -> Result<()> {
    let dir = Path::new(&args.path);
    let toml_path = dir.join(crate::project_config::FILE_NAME);

    if toml_path.exists() && !args.force {
        bail!(crate::out::fail(format!("{} already exists", toml_path.display()), "re-run with --force to overwrite"));
    }

    let project = match args.project {
        Some(p) => p,
        None => {
            if ci_mode.enabled {
                bail!(crate::out::fail("init needs --project in CI/JSON mode", "l8b init --project <id>"));
            }
            let default = default_from_dir(dir);
            dialoguer::Input::<String>::new()
                .with_prompt("Project ID (used as subdomain)")
                .default(default)
                .validate_with(|s: &String| {
                    let ok =
                        !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                    if ok { Ok(()) } else { Err("lowercase letters, digits, hyphens only") }
                })
                .interact_text()?
        }
    };

    // Server binding: explicit flag, else the unambiguous stored login. Left
    // out when ambiguous — the first deploy records it.
    let server = args.server.as_deref().map(crate::config::normalize_server).or_else(|| {
        let store = crate::config::CredentialStore::load();
        let logged_in = store.logged_in_servers();
        if logged_in.len() == 1 {
            Some(logged_in[0].clone())
        } else {
            let default = store.default.clone();
            default.filter(|d| store.get(d).is_some())
        }
    });

    let mut toml = String::new();
    toml.push_str("# LiteBin project config — used by `l8b deploy/env/status` as defaults.\n");
    toml.push_str("# Written by `l8b init`, updated automatically after a successful deploy.\n");
    toml.push_str("# Commit this file — it holds no secrets; env values live on the server (l8b env push).\n");
    toml.push_str(&format!("project = \"{project}\"\n"));
    if let Some(ref n) = args.node {
        toml.push_str(&format!("node = \"{n}\"\n"));
    }
    if let Some(ref s) = server {
        toml.push_str(&format!("server = \"{s}\"\n"));
    }
    std::fs::write(&toml_path, toml)?;
    out.note(&format!("Wrote {}", toml_path.display()));

    if args.mcp {
        let mcp_path = dir.join(".mcp.json");
        if mcp_path.exists() && !args.force {
            out.note(".mcp.json already exists — leaving it as is (use --force to overwrite)");
        } else {
            std::fs::write(
                &mcp_path,
                "{\n  \"mcpServers\": {\n    \"litebin\": {\n      \"command\": \"l8b\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n",
            )?;
            out.note(&format!(
                "Wrote {} (litebin MCP server — picked up by MCP clients that read the workspace .mcp.json)",
                mcp_path.display()
            ));
        }
    }

    if !out.json {
        println!();
        println!("  {} Deploy with: {}", "→".cyan(), "l8b deploy".cyan());
        println!("  {} Status with: {}", "→".cyan(), "l8b status --wait".cyan());
    }
    Ok(())
}

fn default_from_dir(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().replace(['_', ' ', '.'], "-"))
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .unwrap_or_else(|| "my-app".to_string())
}
