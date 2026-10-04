//! `l8b init` — write `l8b.toml` (and optionally the workspace `.mcp.json`)
//! so coding agents working in this repo discover the LiteBin project.

use anyhow::{Result, bail};
use colored::Colorize;
use std::path::Path;

use crate::ci::CiMode;
use crate::out::Out;

pub(crate) struct InitArgs {
    pub project: Option<String>,
    pub port: Option<u16>,
    pub node: Option<String>,
    pub env_file: Option<String>,
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
                bail!(crate::out::fail("init needs --project in CI/JSON mode", "l8b init --project <id> [--port N]"));
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

    let port = match args.port {
        Some(p) => Some(p),
        None => {
            if ci_mode.enabled {
                None
            } else {
                let p: String = dialoguer::Input::new()
                    .with_prompt("App port (enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;
                p.trim().parse::<u16>().ok()
            }
        }
    };

    let mut toml = String::new();
    toml.push_str("# LiteBin project config — used by `l8b deploy/env/status` as defaults.\n");
    toml.push_str(&format!("project = \"{project}\"\n"));
    if let Some(p) = port {
        toml.push_str(&format!("port = {p}\n"));
    }
    if let Some(ref n) = args.node {
        toml.push_str(&format!("node = \"{n}\"\n"));
    }
    if let Some(ref f) = args.env_file {
        toml.push_str(&format!("env_file = \"{f}\"\n"));
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
            out.note(&format!("Wrote {} (litebin MCP server, arrives in a coming release)", mcp_path.display()));
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
