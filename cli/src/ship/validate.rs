use anyhow::Result;
use colored::Colorize;
use dialoguer::Confirm;
use indicatif::ProgressBar;

use crate::auth;

/// Validate compose YAML via orchestrator and return approved capability grants.
/// Prints only what needs attention: the public-service route, capability
/// requests, and unsupported-field errors.
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
    let resp = auth::api_post_json(client, server, "/compose/validate", &body).await?;

    let ok = resp["report"]["ok"].as_bool().unwrap_or(false);
    let findings = resp["report"]["findings"].as_array().cloned().unwrap_or_default();
    let missing: Vec<String> = resp["missing_capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    if !ok {
        println!("  {} Compose compatibility", "!".red());
        for f in findings.iter().filter(|f| f["disposition"] == "unsupported") {
            let path = f["path"].as_str().unwrap_or("?");
            let message = f["message"].as_str().unwrap_or("");
            match f["service"].as_str() {
                Some(svc) => println!("    {} — {}: {}", svc.red(), path, message),
                None => println!("    {}: {}", path, message),
            }
        }
        anyhow::bail!("compose file has unsupported fields — fix them and retry");
    }

    // The one routing fact worth surfacing every deploy (finding text comes
    // from compose-bollard's public-service analysis).
    if let Some(f) = findings.iter().find(|f| {
        f["disposition"] == "translated" && f["message"].as_str().unwrap_or("").contains("is the public HTTP service")
    }) {
        println!("  {} Compose", "::".dimmed());
        println!("     {}", f["message"].as_str().unwrap_or("").dimmed());
    }

    let mut grants: Vec<String> = pregranted.to_vec();
    let still_missing: Vec<String> = missing.into_iter().filter(|c| !grants.iter().any(|g| g == c)).collect();

    if still_missing.is_empty() {
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
