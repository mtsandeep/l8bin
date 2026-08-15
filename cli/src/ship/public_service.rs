use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::Select;

fn parse_container_port(port_str: &str) -> Option<u16> {
    let port_part = port_str.split('/').next().unwrap_or(port_str);
    let container_port = port_part.rsplit(':').next().unwrap_or(port_str);
    container_port.parse().ok()
}

fn service_has_public_label(svc: &serde_yaml::Value) -> bool {
    match svc.get("labels") {
        Some(serde_yaml::Value::Mapping(m)) => {
            m.keys().any(|k| k.as_str().map(|k| k == "litebin.public" || k.ends_with(".public")).unwrap_or(false))
        }
        Some(serde_yaml::Value::Sequence(seq)) => {
            seq.iter().any(|v| v.as_str().map(|s| s.contains("litebin.public")).unwrap_or(false))
        }
        _ => false,
    }
}

fn public_service_candidates(compose: &serde_yaml::Value) -> (Vec<(String, u16)>, bool, bool) {
    let Some(services) = compose.get("services").and_then(|s| s.as_mapping()) else {
        return (Vec::new(), false, false);
    };

    let mut has_public_label = false;
    let mut candidates: Vec<(String, u16)> = Vec::new();
    let mut has_well_known = false;

    for (svc_name, svc) in services {
        if service_has_public_label(svc) {
            has_public_label = true;
        }
        if let Some(port_list) = svc.get("ports").and_then(|p| p.as_sequence()) {
            for port_val in port_list {
                if let Some(port_str) = port_val.as_str()
                    && let Some(p) = parse_container_port(port_str)
                {
                    if p == 80 || p == 443 {
                        has_well_known = true;
                    }
                    if !candidates.iter().any(|(_, ep)| *ep == p) {
                        candidates.push((svc_name.as_str().unwrap_or_default().to_string(), p));
                    }
                }
            }
        }
    }

    (candidates, has_well_known, has_public_label)
}

/// Interactive public-service picker. Returns Some(name) when a label must be injected.
pub(super) fn pick_public_service(compose: &serde_yaml::Value) -> Result<Option<String>> {
    let (candidates, has_well_known, has_public_label) = public_service_candidates(compose);
    if has_public_label || has_well_known || candidates.len() <= 1 {
        return Ok(None);
    }

    let items: Vec<String> = candidates.iter().map(|(name, port)| format!("{} (port {})", name, port)).collect();

    println!("  {} Multiple services expose ports — select the public service", "!".yellow());
    let selection =
        Select::new().with_prompt("Public service (main subdomain entry point)").items(&items).default(0).interact()?;

    Ok(Some(candidates[selection].0.clone()))
}

/// Non-interactive: auto-pick first candidate when ambiguous.
pub(super) fn auto_pick_public_service(compose: &serde_yaml::Value) -> Option<String> {
    let (candidates, has_well_known, has_public_label) = public_service_candidates(compose);
    if has_public_label || has_well_known || candidates.len() <= 1 {
        return None;
    }

    let (name, port) = &candidates[0];
    println!(
        "  {} Multiple services expose ports — auto-selecting {} (port {}) as public",
        "::".dimmed(),
        name.cyan(),
        port
    );
    Some(name.clone())
}

pub(super) fn inject_public_label(yaml: &str, service_name: &str) -> Result<String> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(yaml).with_context(|| "failed to parse compose YAML for label injection")?;

    if let Some(services) = doc.get_mut("services").and_then(|s| s.as_mapping_mut())
        && let Some(svc) = services.get_mut(serde_yaml::Value::String(service_name.to_string()))
        && let Some(svc_map) = svc.as_mapping_mut()
    {
        let labels = svc_map
            .entry(serde_yaml::Value::String("labels".to_string()))
            .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        if let Some(labels_map) = labels.as_mapping_mut() {
            labels_map.insert(
                serde_yaml::Value::String("litebin.public".to_string()),
                serde_yaml::Value::String("true".to_string()),
            );
        }
    }

    serde_yaml::to_string(&doc).with_context(|| "failed to serialize compose YAML after label injection")
}
