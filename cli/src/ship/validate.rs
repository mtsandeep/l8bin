use anyhow::Result;
use colored::Colorize;
use dialoguer::Select;
use indicatif::ProgressBar;

use crate::auth;

/// Grants, required capabilities the user chose to defer (start stays blocked
/// until they are granted), and whether to strip host port bindings.
pub(super) struct CapabilityChoice {
    pub grants: Vec<String>,
    pub deferred: Vec<String>,
    pub strip_host_ports: bool,
}

/// Validate compose YAML via orchestrator and return the capability decision.
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
    predeferred: &[String],
    strip_host_ports: bool,
    spinner: Option<&ProgressBar>,
) -> Result<CapabilityChoice> {
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
    let mut deferred: Vec<String> = predeferred.to_vec();
    let mut strip = strip_host_ports;
    let mut unresolved: Vec<String> = missing
        .into_iter()
        .filter(|c| !(grants.iter().any(|g| g == c) || deferred.iter().any(|d| d == c) || (strip && c == "raw-ports")))
        .collect();

    if unresolved.is_empty() {
        return Ok(CapabilityChoice { grants, deferred, strip_host_ports: strip });
    }

    if !interactive {
        anyhow::bail!(
            "missing required capabilities: {}. Pass --grant-capability for each to grant them, \
             --strip-host-ports to drop host port bindings (raw-ports only), or --defer-capability \
             to stage without granting (start stays blocked until granted)",
            unresolved.join(", ")
        );
    }

    // The prompt and the "requires" lines must not be clobbered by a
    // running spinner (which redraws the current line). Suspend it for the duration.
    let mut ask = || -> Result<()> {
        while !unresolved.is_empty() {
            println!("  {} This compose file requires: {}", "!".yellow(), unresolved.join(", "));
            for f in findings.iter().filter(|f| f["disposition"] == "permission_required") {
                let Some(cap) = f["capability"].as_str() else { continue };
                if !unresolved.iter().any(|c| c == cap) {
                    continue;
                }
                let path = f["path"].as_str().unwrap_or("?");
                let message = f["message"].as_str().unwrap_or("");
                println!("    {} — {}", path.yellow(), message);
            }
            let fixable = unresolved.iter().any(|c| c == "raw-ports");
            let mut choices = vec!["Grant and deploy"];
            if fixable {
                choices.push("Remove host ports — run on the managed network only");
            }
            let defer_idx = choices.len();
            choices.push("Stage without granting — start blocked until granted");
            choices.push("Abort");
            let choice =
                Select::new().with_prompt("How should the deploy proceed?").items(&choices).default(0).interact()?;
            if choice == 0 {
                grants.append(&mut unresolved);
            } else if fixable && choice == 1 {
                strip = true;
                unresolved.retain(|c| c != "raw-ports");
            } else if choice == defer_idx {
                deferred.append(&mut unresolved);
            } else {
                anyhow::bail!("capabilities not granted — aborting deploy");
            }
        }
        Ok(())
    };
    match spinner {
        Some(s) => s.suspend(ask)?,
        None => ask()?,
    }
    Ok(CapabilityChoice { grants, deferred, strip_host_ports: strip })
}
