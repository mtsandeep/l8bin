use anyhow::Result;
use colored::Colorize;
use dialoguer::Confirm;
use indicatif::ProgressBar;

use crate::auth;

/// Validate compose YAML via orchestrator and return approved capability grants.
#[allow(clippy::too_many_arguments)]
pub(super) async fn validate_compose_for_deploy(
    client: &reqwest::Client,
    server: &str,
    project_id: &str,
    yaml: &str,
    is_background: bool,
    interactive: bool,
    pregranted: &[String],
    spinner: Option<&ProgressBar>,
) -> Result<Vec<String>> {
    let body = serde_json::json!({
        "compose": yaml,
        "project_id": project_id,
        "is_background": is_background,
    });
    let resp = auth::session_post(client, server, "/compose/validate", &body).await?;

    let ok = resp["report"]["ok"].as_bool().unwrap_or(false);
    let findings = resp["report"]["findings"].as_array().cloned().unwrap_or_default();
    let missing: Vec<String> = resp["missing_capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    println!("  {} Compose compatibility", "::".dimmed());

    #[derive(Default)]
    struct Groups {
        unsupported: Vec<(Option<String>, String)>,
        permission: Vec<(Option<String>, String)>,
        translated: Vec<(Option<String>, String)>,
        overridden: Vec<(Option<String>, String)>,
        supported_fields: std::collections::BTreeMap<String, Vec<String>>,
        supported_other: Vec<(Option<String>, String)>,
    }
    let mut g = Groups::default();

    for f in &findings {
        let disposition = f["disposition"].as_str().unwrap_or("");
        let path = f["path"].as_str().unwrap_or("?");
        let message = f["message"].as_str().unwrap_or("");
        let service = f["service"].as_str().map(|s| s.to_string());
        match disposition {
            "unsupported" => g.unsupported.push((service, format!("{path} — {message}"))),
            "permission_required" => g.permission.push((service, message.to_string())),
            "translated" => g.translated.push((service, message.to_string())),
            "overridden" => g.overridden.push((service, message.to_string())),
            "supported" => {
                if let (Some(svc), Some(field)) = (service.as_deref(), message.strip_suffix(" is supported")) {
                    g.supported_fields.entry(svc.to_string()).or_default().push(field.to_string());
                } else {
                    g.supported_other.push((service, message.to_string()));
                }
            }
            _ => {}
        }
    }

    let print_grouped = |title: &colored::ColoredString, items: &[(Option<String>, String)]| {
        if items.is_empty() {
            return;
        }
        println!("  {}", title);
        // Preserve order: project-level first, then services alphabetically
        let mut project = Vec::new();
        let mut by_svc: std::collections::BTreeMap<String, Vec<&str>> = std::collections::BTreeMap::new();
        for (svc, line) in items {
            match svc {
                Some(s) => by_svc.entry(s.clone()).or_default().push(line.as_str()),
                None => project.push(line.as_str()),
            }
        }
        for line in project {
            println!("    {}", line.dimmed());
        }
        for (svc, lines) in by_svc {
            println!("    {}", svc.bold());
            for line in lines {
                println!("      {}", line.dimmed());
            }
        }
    };

    print_grouped(&"Unsupported".red().bold(), &g.unsupported);
    print_grouped(&"Capabilities requested".yellow().bold(), &g.permission);

    let mut supported_items: Vec<(Option<String>, String)> = g.supported_other;
    for (svc, fields) in g.supported_fields {
        supported_items.push((Some(svc), fields.join(", ")));
    }
    print_grouped(&"Supported".green().bold(), &supported_items);
    print_grouped(&"Adapted by LiteBin".cyan().bold(), &g.translated);
    print_grouped(&"Overridden by LiteBin".bold(), &g.overridden);

    if !ok {
        anyhow::bail!("compose file has unsupported fields — fix them and retry");
    }

    let mut grants: Vec<String> = pregranted.to_vec();
    let still_missing: Vec<String> = missing.into_iter().filter(|c| !grants.iter().any(|g| g == c)).collect();

    if still_missing.is_empty() {
        if grants.is_empty() {
            println!("  {} All good — no extra capabilities required", "✓".green());
        }
        return Ok(grants);
    }

    if !interactive {
        anyhow::bail!(
            "missing required capabilities: {}. Pass --grant-capability for each (for example, --grant-capability host-network)",
            still_missing.join(", ")
        );
    }

    // The interactive prompt and the "requires" line must not be clobbered by a
    // running spinner (which redraws the current line). Suspend it for the duration.
    let ask = || -> Result<bool> {
        println!("  {} This compose file requires: {}", "!".yellow(), still_missing.join(", "));
        let approve =
            Confirm::new().with_prompt("Grant these capabilities for this project?").default(false).interact()?;
        Ok(approve)
    };
    let approve = match spinner {
        Some(s) => s.suspend(ask)?,
        None => ask()?,
    };
    if !approve {
        anyhow::bail!("capabilities not granted — aborting deploy");
    }
    grants.extend(still_missing);
    Ok(grants)
}
