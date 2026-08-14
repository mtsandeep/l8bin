use super::tables::KNOWN_TOP_LEVEL;
use super::{finding, managed_network_name, CompatibilityFinding, FindingDisposition};

pub(super) fn analyze_top_level(
    root: &serde_yaml::Value,
    project_id: Option<&str>,
    has_host_network: bool,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let Some(map) = root.as_mapping() else {
        return;
    };

    for (key, _val) in map {
        let Some(name) = key.as_str() else {
            continue;
        };
        match name {
            "version" | "name" => findings.push(finding(
                name,
                None,
                FindingDisposition::Supported,
                format!("{name} is accepted and ignored for runtime"),
                None,
            )),
            "services" => {}
            "volumes" => findings.push(finding(
                "volumes",
                None,
                FindingDisposition::Translated,
                "top-level volumes are not created from Compose declarations; bind mounts and relative paths are handled per service",
                None,
            )),
            "networks" => {
                if has_host_network {
                    findings.push(finding(
                        "networks",
                        None,
                        FindingDisposition::Unsupported,
                        "custom networks cannot be combined with host-network services",
                        None,
                    ));
                    continue;
                }
                let msg = match project_id {
                    Some(pid) => format!("top-level networks are ignored; LiteBin creates {}", managed_network_name(pid)),
                    None => {
                        "top-level networks are ignored; LiteBin creates a per-project bridge network".to_string()
                    }
                };
                findings.push(finding(
                    "networks",
                    None,
                    FindingDisposition::Overridden,
                    msg,
                    None,
                ));
            }
            "configs" | "secrets" => findings.push(finding(
                name,
                None,
                FindingDisposition::Overridden,
                format!("top-level {name} are ignored"),
                None,
            )),
            other if other.starts_with("x-") => findings.push(finding(
                other,
                None,
                FindingDisposition::Supported,
                "extension field is ignored",
                None,
            )),
            other if !KNOWN_TOP_LEVEL.contains(&other) => findings.push(finding(
                other,
                None,
                FindingDisposition::Unsupported,
                format!("unknown top-level key '{other}' is not supported"),
                None,
            )),
            _ => {}
        }
    }
}
