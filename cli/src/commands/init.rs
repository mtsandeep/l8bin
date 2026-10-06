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
    pub agents: bool,
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

    // Server binding: explicit flag, else the unambiguous stored login, else
    // an interactive pick (existing logins or connect a new one). CI/JSON
    // falls back to the last-used default; the first deploy records it.
    let server = pick_init_server(args.server.as_deref(), ci_mode).await?;

    let toml = crate::project_config::render(Some(&project), args.node.as_deref(), server.as_deref());
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

    if args.agents {
        let agents_path = dir.join(crate::agents_md::FILE_NAME);
        if crate::agents_md::merge_section(dir)? {
            out.note(&format!("Wrote {} (LiteBin section; existing content kept)", agents_path.display()));
        } else {
            out.note(&format!("{} already up to date", agents_path.display()));
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

/// Resolve the server to bind: flag > the only login > interactive pick over
/// existing logins or a new pairing. CI/JSON keeps the last-used default.
async fn pick_init_server(flag: Option<&str>, ci_mode: &CiMode) -> Result<Option<String>> {
    if let Some(s) = flag {
        return Ok(Some(crate::config::normalize_server(s)));
    }
    let store = crate::config::CredentialStore::load();
    let logged_in = store.logged_in_servers();
    if logged_in.len() == 1 {
        return Ok(Some(logged_in[0].clone()));
    }
    if ci_mode.enabled {
        return Ok(store.default.clone().filter(|d| store.get(d).is_some()));
    }

    let server = if logged_in.is_empty() {
        dialoguer::Input::<String>::new().with_prompt("Server URL").interact_text()?
    } else {
        let mut items: Vec<String> = logged_in.iter().map(|s| s.to_string()).collect();
        items.push("Connect a new server…".into());
        let choice = dialoguer::Select::new()
            .with_prompt("Deploy this project to which server?")
            .items(&items)
            .default(0)
            .interact()?;
        if choice < logged_in.len() {
            logged_in[choice].clone()
        } else {
            dialoguer::Input::<String>::new().with_prompt("Server URL").interact_text()?
        }
    };
    crate::auth::login(&server, "manage").await?;
    Ok(Some(crate::config::normalize_server(&server)))
}
