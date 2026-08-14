use std::collections::HashSet;

use crate::parse::ComposeService;

use super::helpers::{
    bind_source_exposes_docker_socket, finding, is_docker_socket_source, is_repo_relative_bind, managed_container_name,
    managed_network_name, volume_source,
};
use super::tables::{IGNORED_SERVICE_FIELDS, SUPPORTED_SERVICE_FIELDS, UNSUPPORTED_SERVICE_FIELDS};
use super::{CompatibilityFinding, FindingDisposition};

pub(super) fn analyze_service(
    svc_name: &str,
    svc: &ComposeService,
    public_service: Option<&str>,
    project_id: Option<&str>,
    is_background: bool,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let prefix = format!("services.{svc_name}");
    let is_public = public_service == Some(svc_name);
    if svc_name == "litebin-docker-proxy" {
        findings.push(finding(
            &prefix,
            Some(svc_name.into()),
            FindingDisposition::Unsupported,
            "service name 'litebin-docker-proxy' is reserved for LiteBin's managed Docker observation proxy",
            None,
        ));
    }
    let host_network = svc.uses_host_network();
    if host_network {
        findings.push(finding(
            format!("{prefix}.network_mode"),
            Some(svc_name.into()),
            if is_background { FindingDisposition::PermissionRequired } else { FindingDisposition::Unsupported },
            if is_background {
                "host networking runs the service in the host network namespace and exposes listeners directly"
            } else {
                "host networking is available only to background projects without managed HTTP ingress"
            },
            if is_background { Some("host-network") } else { None },
        ));
        if svc.ports.as_ref().is_some_and(|ports| !ports.is_empty()) {
            findings.push(finding(
                format!("{prefix}.ports"),
                Some(svc_name.into()),
                FindingDisposition::Unsupported,
                "Compose ports cannot be combined with host networking; listeners bind directly on the host",
                None,
            ));
        }
    }

    // Supported fields that are present
    let present_supported: &[(&str, bool)] = &[
        ("image", svc.image.is_some()),
        ("build", svc.build.is_some()),
        ("command", svc.command.is_some()),
        ("entrypoint", svc.entrypoint.is_some()),
        ("working_dir", svc.working_dir.is_some()),
        ("user", svc.user.is_some()),
        ("environment", svc.environment.is_some()),
        ("labels", svc.labels.is_some()),
        ("ports", svc.ports.is_some()),
        ("depends_on", svc.depends_on.is_some()),
        ("volumes", svc.volumes.is_some()),
        ("healthcheck", svc.healthcheck.is_some()),
        ("shm_size", svc.shm_size.is_some()),
        ("tmpfs", svc.tmpfs.is_some()),
        ("read_only", svc.read_only.is_some()),
        ("extra_hosts", svc.extra_hosts.is_some()),
        ("memory", svc.memory.is_some()),
        ("cpus", svc.cpus.is_some()),
        ("cap_add", svc.cap_add.is_some()),
        ("cap_drop", svc.cap_drop.is_some()),
        ("stdin_open", svc.stdin_open.is_some()),
        ("tty", svc.tty.is_some()),
        ("restart", svc.restart.is_some()),
    ];

    for (field, present) in present_supported {
        if !*present {
            continue;
        }
        match *field {
            "ports" if !host_network => analyze_ports(svc_name, svc, is_public, findings),
            "ports" => {}
            "volumes" => analyze_volumes(svc_name, svc, findings),
            "restart" => findings.push(finding(
                format!("{prefix}.restart"),
                Some(svc_name.into()),
                FindingDisposition::Translated,
                "restart is accepted when set; otherwise LiteBin manages lifecycle (default restart: no)",
                None,
            )),
            "cap_add" | "cap_drop" => findings.push(finding(
                format!("{prefix}.{field}"),
                Some(svc_name.into()),
                FindingDisposition::Overridden,
                format!(
                    "{field} from Compose is merged then overridden by LiteBin's security profile (ALL dropped, curated set added)"
                ),
                None,
            )),
            "build" => findings.push(finding(
                format!("{prefix}.build"),
                Some(svc_name.into()),
                FindingDisposition::Translated,
                "build context is built by the CLI and the resulting image is deployed",
                None,
            )),
            _ => findings.push(finding(
                format!("{prefix}.{field}"),
                Some(svc_name.into()),
                FindingDisposition::Supported,
                format!("{field} is supported"),
                None,
            )),
        }
    }

    if svc.image.is_none() && svc.build.is_none() {
        findings.push(finding(
            prefix.clone(),
            Some(svc_name.into()),
            FindingDisposition::Unsupported,
            "service must define image or build",
            None,
        ));
    }

    // Known ignored / unsupported named fields from `extra`
    let mut handled_extra: HashSet<String> = HashSet::new();

    for (field, disposition, message) in IGNORED_SERVICE_FIELDS {
        if svc.extra.contains_key(*field) {
            let msg = if *field == "container_name" {
                match project_id {
                    Some(pid) => format!(
                        "container_name is overridden; LiteBin will name this container {}",
                        managed_container_name(pid, svc_name)
                    ),
                    None => (*message).to_string(),
                }
            } else {
                (*message).to_string()
            };
            findings.push(finding(format!("{prefix}.{field}"), Some(svc_name.into()), *disposition, msg, None));
            handled_extra.insert((*field).to_string());
        }
    }

    for (field, message) in UNSUPPORTED_SERVICE_FIELDS {
        if *field == "network_mode" {
            if svc.extra.contains_key(*field) && !host_network {
                findings.push(finding(
                    format!("{prefix}.{field}"),
                    Some(svc_name.into()),
                    FindingDisposition::Unsupported,
                    "only network_mode: host is supported, and it requires the host-network capability",
                    None,
                ));
            }
            if svc.extra.contains_key(*field) {
                handled_extra.insert((*field).to_string());
            }
            continue;
        }
        if svc.extra.contains_key(*field) {
            let msg = match (*field, project_id) {
                ("network_mode", Some(pid)) => format!(
                    "network_mode is not applied yet; LiteBin always uses managed network {}",
                    managed_network_name(pid)
                ),
                ("networks", Some(pid)) => {
                    format!("custom Compose networks are ignored; LiteBin creates {}", managed_network_name(pid))
                }
                ("networks", None) => {
                    "custom Compose networks are ignored; LiteBin creates a per-project bridge network".to_string()
                }
                _ => (*message).to_string(),
            };
            findings.push(finding(
                format!("{prefix}.{field}"),
                Some(svc_name.into()),
                FindingDisposition::Unsupported,
                msg,
                None,
            ));
            handled_extra.insert((*field).to_string());
        }
    }

    // Any remaining unknown service keys
    let mut unknown: Vec<_> = svc
        .extra
        .keys()
        .filter(|k| !handled_extra.contains(*k) && !SUPPORTED_SERVICE_FIELDS.contains(&k.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    for key in unknown {
        if key.starts_with("x-") {
            findings.push(finding(
                format!("{prefix}.{key}"),
                Some(svc_name.into()),
                FindingDisposition::Supported,
                "extension field is ignored",
                None,
            ));
        } else {
            findings.push(finding(
                format!("{prefix}.{key}"),
                Some(svc_name.into()),
                FindingDisposition::Unsupported,
                format!("unknown service field '{key}' is not supported"),
                None,
            ));
        }
    }
}

fn analyze_ports(svc_name: &str, svc: &ComposeService, is_public: bool, findings: &mut Vec<CompatibilityFinding>) {
    let prefix = format!("services.{svc_name}.ports");
    let Some(ports) = &svc.ports else {
        return;
    };

    let exposed = svc.exposed_ports();
    let has_udp = exposed.iter().any(|(_, proto)| proto == "udp");
    let has_host_mapping = ports.iter().any(|p| p.contains(':'));

    if is_public {
        findings.push(finding(
            &prefix,
            Some(svc_name.into()),
            FindingDisposition::Translated,
            "public service ports are translated to LiteBin managed HTTP ingress (loopback bind + Caddy route), not published as written",
            None,
        ));
        // Extra published ports beyond the primary HTTP container port need raw-ports.
        if has_udp || exposed.len() > 1 {
            findings.push(finding(
                &prefix,
                Some(svc_name.into()),
                FindingDisposition::PermissionRequired,
                "additional or non-HTTP published ports require the raw-ports capability",
                Some("raw-ports"),
            ));
        } else if has_host_mapping {
            // Single HTTP mapping like 8080:80 — host side is ignored intentionally.
            findings.push(finding(
                &prefix,
                Some(svc_name.into()),
                FindingDisposition::Translated,
                "host port side of the mapping is ignored; LiteBin assigns a managed loopback port",
                None,
            ));
        }
    } else {
        findings.push(finding(
            &prefix,
            Some(svc_name.into()),
            FindingDisposition::PermissionRequired,
            "host port publishing requires the raw-ports capability (non-public services do not get HTTP ingress)",
            Some("raw-ports"),
        ));
        findings.push(finding(
            &prefix,
            Some(svc_name.into()),
            FindingDisposition::Translated,
            "without raw-ports, declared ports are not bound on the host",
            None,
        ));
    }
}

fn analyze_volumes(svc_name: &str, svc: &ComposeService, findings: &mut Vec<CompatibilityFinding>) {
    let prefix = format!("services.{svc_name}.volumes");
    let Some(volumes) = &svc.volumes else {
        return;
    };

    findings.push(finding(
        &prefix,
        Some(svc_name.into()),
        FindingDisposition::Supported,
        "bind mounts and named volumes are supported (relative binds are remapped under the project directory)",
        None,
    ));

    for vol in volumes {
        let source = volume_source(vol);
        if is_docker_socket_source(source) {
            findings.push(finding(
                format!("{prefix} ({vol})"),
                Some(svc_name.into()),
                FindingDisposition::PermissionRequired,
                "Docker socket declarations require an explicit docker-observe grant; read-only mount syntax does not make the Docker API safe",
                Some("docker-observe"),
            ));
            findings.push(finding(
                format!("{prefix} ({vol})"),
                Some(svc_name.into()),
                FindingDisposition::Translated,
                "the raw socket is always removed; with docker-observe, DOCKER_HOST points to LiteBin's endpoint-allowlisted read-only proxy",
                None,
            ));
        } else if bind_source_exposes_docker_socket(source) {
            findings.push(finding(
                format!("{prefix} ({vol})"),
                Some(svc_name.into()),
                FindingDisposition::Unsupported,
                "host bind contains the Docker daemon socket; mount a declared docker.sock path and grant docker-observe instead",
                None,
            ));
        } else if is_repo_relative_bind(source) {
            findings.push(finding(
                format!("{prefix} ({vol})"),
                Some(svc_name.into()),
                FindingDisposition::Translated,
                "relative bind path is remapped under the project directory, but its contents are NOT transferred from your repo to the node; ensure the file exists on the node, or bake it into the image with a `build:` context",
                None,
            ));
        }
    }
}
