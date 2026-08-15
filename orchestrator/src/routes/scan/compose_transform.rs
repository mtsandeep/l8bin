/// Resolve a compose.yaml by replacing `build:` directives with `image:` using the
/// actual image from each running container — the same transformation `l8b ship` does.
///
/// For services that have `build:` but no `image:`, we look up the container's
/// current image (from Docker inspect) and inject it. The `build:` key is removed.
/// Services that already have `image:` are left untouched.
pub(super) fn resolve_compose_yaml(raw: String, containers: &[litebin_common::scan::ScanContainer]) -> String {
    // Build lookup maps from scan data
    let image_map: std::collections::HashMap<&str, &str> =
        containers.iter().map(|c| (c.service_name.as_str(), c.image.as_str())).collect();

    // Map: (service_name, container_destination) → absolute host source
    // Used to rewrite relative bind mounts to absolute paths.
    let bind_mount_map: std::collections::HashMap<(&str, &str), &str> = containers
        .iter()
        .flat_map(|c| {
            c.volumes.iter().filter_map(move |v| {
                if v.volume_type == "bind" {
                    Some((c.service_name.as_str(), v.destination.as_str(), v.source.as_str()))
                } else {
                    None
                }
            })
        })
        .map(|(svc, dest, src)| ((svc, dest), src))
        .collect();

    let mut compose: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            // On Windows, compose files may have unescaped backslashes in quoted
            // strings (e.g. "D:\dev\foo:/bar").  Retry with backslashes normalized.
            let normalized = raw.replace('\\', "/");
            match serde_yaml::from_str(&normalized) {
                Ok(v) => v,
                Err(e2) => {
                    tracing::warn!(error = %e, retry_error = %e2, "resolve: failed to parse compose YAML, returning as-is");
                    return raw;
                }
            }
        }
    };

    let services = match compose.get_mut("services").and_then(|s| s.as_mapping_mut()) {
        Some(m) => m,
        None => return serde_yaml::to_string(&compose).unwrap_or(raw),
    };

    for (svc_key, svc_val) in services.iter_mut() {
        let svc_name = svc_key.as_str().unwrap_or_default();
        let svc_map = match svc_val.as_mapping_mut() {
            Some(m) => m,
            None => continue,
        };

        // ── Replace build: → image: ────────────────────────────────────────
        let has_build = svc_map.contains_key(serde_yaml::Value::String("build".into()));
        let has_image = svc_map.contains_key(serde_yaml::Value::String("image".into()));

        if has_build
            && !has_image
            && let Some(&image) = image_map.get(svc_name)
        {
            svc_map.remove(serde_yaml::Value::String("build".into()));
            svc_map.insert(serde_yaml::Value::String("image".into()), serde_yaml::Value::String(image.to_string()));
            tracing::info!(
                service = svc_name,
                image = image,
                "resolve: replaced build: with image: from running container"
            );
        }

        // ── Rewrite relative bind mounts → absolute paths ──────────────────
        // The original compose may have `./data:/var/lib/data` relative to its
        // own working dir.  When stored under projects/<id>/, those relatives
        // would point to the wrong place.  Docker inspect gives us the resolved
        // absolute host path, so we substitute it in.
        if let Some(volumes_val) = svc_map.get_mut(serde_yaml::Value::String("volumes".into()))
            && let Some(vols) = volumes_val.as_sequence_mut()
        {
            for vol_entry in vols.iter_mut() {
                // Handle both string form ("src:dst") and mapping form
                let source_key = serde_yaml::Value::String("source".into());
                let is_bind = vol_entry
                    .as_mapping()
                    .map(|m| m.get(serde_yaml::Value::String("type".into())).and_then(|t| t.as_str()) == Some("bind"))
                    .unwrap_or(false);

                if is_bind {
                    // Long-form: { type: bind, source: "./data", target: "/var/lib/data" }
                    if let Some(m) = vol_entry.as_mapping_mut() {
                        let src = m.get(&source_key).and_then(|v| v.as_str()).map(|s| s.to_string());
                        let target = m
                            .get(serde_yaml::Value::String("target".into()))
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string());
                        if let Some(ref src_str) = src
                            && (src_str.starts_with('.') || src_str.starts_with(".."))
                            && let Some(ref dest_str) = target
                            && let Some(&abs_source) = bind_mount_map.get(&(svc_name, dest_str.as_str()))
                        {
                            let normalized = abs_source.replace('\\', "/");
                            m.insert(source_key.clone(), serde_yaml::Value::String(normalized));
                            tracing::debug!(
                                service = svc_name,
                                old = src_str,
                                new = abs_source,
                                "resolve: rewrote relative bind mount to absolute"
                            );
                        }
                    }
                } else {
                    // Short-form: "./data:/var/lib/data" or "./data:/var/lib/data:rw"
                    let vol_str = vol_entry.as_str().map(|s| s.to_string());
                    if let Some(ref vol_s) = vol_str {
                        let parts: Vec<&str> = vol_s.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            let src = parts[0];
                            if src.starts_with('.') || src.starts_with("..") {
                                let dest_parts: Vec<&str> = parts[1].split(':').collect();
                                let dest = dest_parts[0];
                                if let Some(&abs_source) = bind_mount_map.get(&(svc_name, dest)) {
                                    let normalized = abs_source.replace('\\', "/");
                                    let new_vol = format!("{}:{}", normalized, parts[1]);
                                    *vol_entry = serde_yaml::Value::String(new_vol);
                                    tracing::debug!(
                                        service = svc_name,
                                        old = src,
                                        new = abs_source,
                                        "resolve: rewrote relative bind mount to absolute"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    serde_yaml::to_string(&compose).unwrap_or(raw)
}

/// Reconstruct a minimal compose.yaml from container inspect data.
/// Used as fallback when no compose.yaml is available.
pub(super) fn reconstruct_compose_yaml(containers: &[litebin_common::scan::ScanContainer]) -> String {
    let mut yaml = String::from("services:\n");
    for c in containers {
        yaml.push_str(&format!("  {}:\n", c.service_name));
        yaml.push_str(&format!("    image: {}\n", c.image));

        // Ports (include all ports, not just externally published ones)
        if !c.ports.is_empty() {
            yaml.push_str("    ports:\n");
            for p in &c.ports {
                if let Some(ext) = p.external {
                    yaml.push_str(&format!("      - \"{}:{}\"\n", ext, p.internal));
                } else {
                    yaml.push_str(&format!("      - \"{}\"\n", p.internal));
                }
            }
        }

        // Volumes — normalize Windows backslashes to forward slashes and quote
        // paths that contain colons (e.g. Windows paths like D:/foo:/bar)
        if !c.volumes.is_empty() {
            yaml.push_str("    volumes:\n");
            for v in &c.volumes {
                let source = v.source.replace('\\', "/");
                let spec = format!("{}:{}", source, v.destination);
                if spec.contains(':') && !spec.starts_with('"') {
                    yaml.push_str(&format!("      - \"{}\"\n", spec));
                } else {
                    yaml.push_str(&format!("      - {}\n", spec));
                }
            }
        }
    }
    yaml
}
