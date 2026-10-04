//! `l8b setup` — first-run bootstrap against a fresh server: create the admin
//! account (only possible when no users exist), then pair via the dashboard.

use anyhow::{Result, bail};
use colored::Colorize;

use crate::auth;
use crate::ci::CiMode;
use crate::out::Out;

pub(crate) async fn run(server: &str, ci_mode: &CiMode, out: &Out) -> Result<()> {
    let server = if server.starts_with("http://") || server.starts_with("https://") {
        server.to_string()
    } else {
        format!("https://{server}")
    };
    let server = server.trim_end_matches('/').to_string();

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{server}/auth/setup"))
        .send()
        .await
        .map_err(|e| crate::out::fail(format!("server unreachable: {e}"), "check the URL and server status"))?;
    let setup: serde_json::Value = resp.json().await.unwrap_or_default();
    let needs_setup = setup["needs_setup"].as_bool().unwrap_or(false);

    if needs_setup {
        if ci_mode.enabled {
            bail!(crate::out::fail(
                "server has no admin user; creating one is interactive",
                "run `l8b setup --server <url>` in a terminal"
            ));
        }
        out.note("No admin account exists yet — creating one now.");
        let username: String = dialoguer::Input::new().with_prompt("Admin username").interact_text()?;
        let password = dialoguer::Password::new().with_prompt("Admin password").interact()?;
        let confirm = dialoguer::Password::new().with_prompt("Confirm password").interact()?;
        if password != confirm {
            bail!("passwords do not match");
        }

        let resp = client
            .post(format!("{server}/auth/register"))
            .json(&serde_json::json!({"username": username, "password": password}))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("admin creation failed ({status}): {body}");
        }
        println!("  {} Admin created.", "✓".green());
    } else {
        out.note("Server already has an admin account.");
    }

    println!();
    out.note("Pairing this machine (approve from the dashboard):");
    auth::login(&server, "manage").await
}
