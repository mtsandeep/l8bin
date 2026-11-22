//! Host-port binding removal: rewrite explicit `HOST:CONTAINER` mappings to
//! container-only short syntax so a compose stops requiring `raw-ports`.

use serde_yaml::Value;

/// Result of a host-port strip.
pub struct HostPortStrip {
    pub yaml: String,
    /// One entry per rewritten mapping: (service, original spec).
    pub removed: Vec<(String, String)>,
}

/// Remove the host side of every explicit `ports:` mapping — `"8080:8080"`
/// becomes `"8080"`, long-syntax `published` keys are dropped. Container-only
/// entries are left untouched, and when nothing is removed the original text
/// (comments and formatting included) is returned as-is.
pub fn strip_host_port_bindings(yaml: &str) -> crate::Result<HostPortStrip> {
    let mut root: Value = serde_yaml::from_str(yaml)?;
    let mut removed: Vec<(String, String)> = Vec::new();

    let Some(Value::Mapping(services)) = root.get_mut("services") else {
        return Ok(HostPortStrip { yaml: yaml.to_string(), removed });
    };

    for (service, svc) in services.iter_mut() {
        let service_name = service.as_str().unwrap_or("?").to_string();
        let Some(Value::Sequence(ports)) = svc.get_mut("ports") else { continue };
        for entry in ports.iter_mut() {
            match entry {
                Value::String(spec) => {
                    let Some(container_only) = container_only_spec(spec) else { continue };
                    removed.push((service_name.clone(), spec.clone()));
                    *spec = container_only;
                }
                Value::Mapping(long) => {
                    if long.remove("published").is_some() {
                        let target = long
                            .get("target")
                            .and_then(Value::as_u64)
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "?".into());
                        removed.push((service_name.clone(), format!("published:{target}")));
                    }
                }
                _ => {}
            }
        }
    }

    if removed.is_empty() {
        return Ok(HostPortStrip { yaml: yaml.to_string(), removed });
    }
    let out = serde_yaml::to_string(&root)?;
    Ok(HostPortStrip { yaml: out, removed })
}

/// `"8080:8080/udp"` → `"8080/udp"`; specs without a host side return `None`.
/// The last `:`-separated segment is the container port, which also holds for
/// IP-prefixed (`127.0.0.1:8080:8080`) and bracketed IPv6 forms.
fn container_only_spec(spec: &str) -> Option<String> {
    let (port_part, protocol) = match spec.split_once('/') {
        Some((p, proto)) => (p, Some(proto)),
        None => (spec, None),
    };
    if !port_part.contains(':') {
        return None;
    }
    let container = port_part.rsplit(':').next()?.trim();
    if container.is_empty() {
        return None;
    }
    Some(match protocol {
        Some(proto) => format!("{container}/{proto}"),
        None => container.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::strip_host_port_bindings;
    use crate::analyze_compose_yaml_for_workload;

    fn required_caps(yaml: &str) -> Vec<String> {
        analyze_compose_yaml_for_workload(yaml, None, Some("proj"), false).unwrap().1.required_capabilities
    }

    #[test]
    fn explicit_mappings_become_container_only() {
        let strip = strip_host_port_bindings(
            "services:\n  backend:\n    image: nginx\n    ports:\n      - \"8080:8080\"\n      - \"9090:9090/udp\"\n",
        )
        .unwrap();
        assert_eq!(strip.removed.len(), 2);
        assert!(!strip.yaml.contains("8080:8080"));
        assert!(!strip.yaml.contains("9090:9090"));
        assert!(!required_caps(&strip.yaml).contains(&"raw-ports".to_string()));
    }

    #[test]
    fn ip_prefixed_and_ipv6_forms_keep_container_side() {
        let strip = strip_host_port_bindings(
            "services:\n  a:\n    image: nginx\n    ports:\n      - \"127.0.0.1:8080:8080\"\n      - \"[::1]:8080:8080\"\n",
        )
        .unwrap();
        assert_eq!(strip.removed.len(), 2);
        assert!(!strip.yaml.contains("127.0.0.1"));
        assert!(!strip.yaml.contains("[::1]"));
        assert!(!required_caps(&strip.yaml).contains(&"raw-ports".to_string()));
    }

    #[test]
    fn long_syntax_published_key_is_dropped() {
        let strip = strip_host_port_bindings(
            "services:\n  a:\n    image: nginx\n    ports:\n      - target: 8080\n        published: 18080\n        protocol: tcp\n",
        )
        .unwrap();
        assert_eq!(strip.removed, vec![("a".to_string(), "published:8080".to_string())]);
        assert!(!strip.yaml.contains("published"));
        assert!(!strip.yaml.contains("18080"));
    }

    #[test]
    fn container_only_and_absent_ports_are_untouched() {
        let yaml = "services:\n  a:\n    image: nginx\n    ports:\n      - \"8080\"\n      - \"9090/udp\"\n  b:\n    image: postgres\n";
        let strip = strip_host_port_bindings(yaml).unwrap();
        assert!(strip.removed.is_empty());
        assert_eq!(strip.yaml, yaml);
    }

    #[test]
    fn raw_ports_requirement_disappears_end_to_end() {
        let yaml = "services:\n  ui:\n    image: nginx\n    ports:\n      - \"3000:80\"\n  backend:\n    image: nginx\n    ports:\n      - \"8080:8080\"\n";
        assert!(required_caps(yaml).contains(&"raw-ports".to_string()));
        let strip = strip_host_port_bindings(yaml).unwrap();
        assert_eq!(strip.removed.len(), 2);
        assert!(!required_caps(&strip.yaml).contains(&"raw-ports".to_string()));
    }
}
