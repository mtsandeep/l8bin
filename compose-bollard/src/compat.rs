use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{ComposeError, Result};
use crate::parse::{ComposeFile, ComposeService};
use crate::ComposeParser;

/// How LiteBin handles a Compose field or feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingDisposition {
    /// Applied as written (or with only mechanical normalization).
    Supported,
    /// Kept, but LiteBin rewrites or remaps the meaning.
    Translated,
    /// Present in Compose but replaced by LiteBin policy.
    Overridden,
    /// Requires an explicit project capability grant before deploy.
    PermissionRequired,
    /// Not implemented — deploy must fail until the file is changed.
    Unsupported,
}

/// One compatibility finding for a Compose path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityFinding {
    /// YAML-ish path, e.g. `services.agent.network_mode`.
    pub path: String,
    /// Service name when the finding is service-scoped.
    pub service: Option<String>,
    pub disposition: FindingDisposition,
    pub message: String,
    /// Capability id when `disposition` is `PermissionRequired`.
    pub capability: Option<String>,
}

/// Structured Compose compatibility report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub findings: Vec<CompatibilityFinding>,
    /// False when any finding is `Unsupported`.
    pub ok: bool,
    /// Unique capability ids requested by this Compose file.
    pub required_capabilities: Vec<String>,
}

impl CompatibilityReport {
    pub fn unsupported(&self) -> impl Iterator<Item = &CompatibilityFinding> {
        self.findings
            .iter()
            .filter(|f| f.disposition == FindingDisposition::Unsupported)
    }

    pub fn permission_required(&self) -> impl Iterator<Item = &CompatibilityFinding> {
        self.findings
            .iter()
            .filter(|f| f.disposition == FindingDisposition::PermissionRequired)
    }
}

/// Known top-level Compose keys LiteBin may encounter.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "version",
    "name",
    "services",
    "volumes",
    "networks",
    "configs",
    "secrets",
    "x-litebin",
];

/// Service fields LiteBin maps into Bollard / runtime config.
const SUPPORTED_SERVICE_FIELDS: &[&str] = &[
    "image",
    "build",
    "command",
    "entrypoint",
    "working_dir",
    "user",
    "environment",
    "labels",
    "ports",
    "depends_on",
    "volumes",
    "healthcheck",
    "shm_size",
    "tmpfs",
    "read_only",
    "extra_hosts",
    "memory",
    "cpus",
    "cap_add",
    "cap_drop",
    "stdin_open",
    "tty",
    "restart",
];

/// Service fields that are recognized but not yet implemented.
const UNSUPPORTED_SERVICE_FIELDS: &[(&str, &str)] = &[
    (
        "network_mode",
        "network_mode is not applied yet; LiteBin always uses a managed project bridge network",
    ),
    (
        "networks",
        "custom Compose networks are ignored; LiteBin creates a per-project bridge network",
    ),
    (
        "privileged",
        "privileged mode is not supported",
    ),
    (
        "pid",
        "pid namespace sharing is not supported",
    ),
    (
        "devices",
        "device mounts are not supported",
    ),
    (
        "ipc",
        "IPC namespace sharing is not supported",
    ),
    (
        "uts",
        "UTS namespace options are not supported",
    ),
    (
        "runtime",
        "custom container runtimes are not supported",
    ),
    (
        "cgroup_parent",
        "cgroup_parent is not supported",
    ),
    (
        "sysctls",
        "sysctls are not supported",
    ),
];

/// Soft-ignored service fields (informational unsupported / overridden).
const IGNORED_SERVICE_FIELDS: &[(&str, FindingDisposition, &str)] = &[
    (
        "container_name",
        FindingDisposition::Overridden,
        "container_name is overridden; LiteBin names containers litebin-<project>.<service>",
    ),
    (
        "env_file",
        FindingDisposition::Overridden,
        "env_file is not loaded from Compose; provide runtime env via LiteBin secrets / .env.l8bin instead",
    ),
    (
        "logging",
        FindingDisposition::Overridden,
        "logging is overridden by LiteBin (json-file, 10m × 3)",
    ),
    (
        "deploy",
        FindingDisposition::Overridden,
        "deploy.* (Swarm) is ignored; use top-level memory/cpus for resource limits",
    ),
    (
        "profiles",
        FindingDisposition::Overridden,
        "Compose profiles are ignored; all services in the file are deployed",
    ),
    (
        "security_opt",
        FindingDisposition::Overridden,
        "security_opt is overridden by LiteBin (no-new-privileges)",
    ),
    (
        "dns",
        FindingDisposition::Overridden,
        "custom DNS is ignored",
    ),
    (
        "hostname",
        FindingDisposition::Overridden,
        "hostname is ignored",
    ),
    (
        "domainname",
        FindingDisposition::Overridden,
        "domainname is ignored",
    ),
    (
        "platform",
        FindingDisposition::Overridden,
        "platform is ignored from Compose; build/pull uses the target node architecture",
    ),
    (
        "ulimits",
        FindingDisposition::Overridden,
        "ulimits are ignored",
    ),
    (
        "expose",
        FindingDisposition::Translated,
        "expose is ignored for routing; use ports (or LiteBin public service selection) for HTTP ingress",
    ),
    (
        "init",
        FindingDisposition::Overridden,
        "init: true is ignored",
    ),
    (
        "stop_grace_period",
        FindingDisposition::Overridden,
        "stop_grace_period is ignored; LiteBin manages stop timeouts",
    ),
    (
        "stop_signal",
        FindingDisposition::Overridden,
        "stop_signal is ignored",
    ),
];

fn finding(
    path: impl Into<String>,
    service: Option<String>,
    disposition: FindingDisposition,
    message: impl Into<String>,
    capability: Option<&str>,
) -> CompatibilityFinding {
    CompatibilityFinding {
        path: path.into(),
        service,
        disposition,
        message: message.into(),
        capability: capability.map(|s| s.to_string()),
    }
}

fn is_docker_sock_source(source: &str) -> bool {
    matches!(
        normalize_unix_path(source).as_deref(),
        Some("/var/run/docker.sock" | "/run/docker.sock")
    )
}

fn docker_socket_is_below(source: &str) -> bool {
    let Some(source) = normalize_unix_path(source) else {
        return false;
    };
    ["/var/run/docker.sock", "/run/docker.sock"]
        .iter()
        .any(|socket| source == "/" || socket.starts_with(&format!("{source}/")))
}

fn normalize_unix_path(path: &str) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value),
        }
    }
    Some(format!("/{}", components.join("/")))
}

fn volume_source(volume: &str) -> &str {
    volume.split(':').next().unwrap_or(volume).trim()
}

/// Docker container name LiteBin will assign (matches `litebin_common::container_name`).
fn managed_container_name(project_id: &str, service: &str) -> String {
    if service == "web" {
        format!("litebin-{project_id}")
    } else {
        format!("litebin-{project_id}.{service}")
    }
}

fn managed_network_name(project_id: &str) -> String {
    format!("litebin-{project_id}")
}

/// Analyze a Compose YAML string for LiteBin compatibility.
///
/// `public_service` is the service LiteBin will treat as the HTTP ingress target
/// (label / port / CLI selection). Pass `None` to use automatic detection.
///
/// When `project_id` is provided, findings use concrete names
/// (e.g. `litebin-beszel.beszel-agent`) instead of placeholders.
pub fn analyze_compose_yaml(
    yaml: &str,
    public_service: Option<&str>,
    project_id: Option<&str>,
) -> Result<(ComposeFile, CompatibilityReport)> {
    let root: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    let compose = ComposeParser::parse(yaml)?;
    let report = analyze_compose(&root, &compose, public_service, project_id)?;
    Ok((compose, report))
}

/// Analyze an already-parsed Compose file plus its raw YAML root.
pub fn analyze_compose(
    root: &serde_yaml::Value,
    compose: &ComposeFile,
    public_service: Option<&str>,
    project_id: Option<&str>,
) -> Result<CompatibilityReport> {
    let mut findings = Vec::new();

    analyze_top_level(root, project_id, &mut findings);

    if compose.services.is_empty() {
        return Err(ComposeError::NoServices);
    }

    // Validate graph early so callers get a clear error, but still build findings.
    let _ = compose.topological_sort()?;

    let detected_public = match public_service {
        Some(name) => {
            if !compose.services.contains_key(name) {
                return Err(ComposeError::ServiceNotFound {
                    name: name.to_string(),
                });
            }
            Some(name.to_string())
        }
        None => compose.detect_public_service()?,
    };

    if let Some(ref pub_svc) = detected_public {
        findings.push(finding(
            format!("services.{pub_svc}"),
            Some(pub_svc.clone()),
            FindingDisposition::Translated,
            format!(
                "'{pub_svc}' is the public HTTP service; LiteBin will route {pub_svc}.{{domain}} to it"
            ),
            None,
        ));
    } else {
        findings.push(finding(
            "services",
            None,
            FindingDisposition::Supported,
            "no public HTTP service detected; LiteBin will not create a managed ingress route from ports alone",
            None,
        ));
    }

    let mut service_names: Vec<_> = compose.services.keys().cloned().collect();
    service_names.sort();

    for svc_name in &service_names {
        let svc = &compose.services[svc_name];
        analyze_service(
            svc_name,
            svc,
            detected_public.as_deref(),
            project_id,
            &mut findings,
        );
    }

    // Always note LiteBin security overrides once (project-level).
    findings.push(finding(
        "litebin.security",
        None,
        FindingDisposition::Overridden,
        "LiteBin applies capability drop/add, no-new-privileges, pids_limit, and log rotation to all services",
        None,
    ));
    let network_msg = match project_id {
        Some(pid) => format!(
            "services join managed network {} (Compose networks / network_mode are not applied)",
            managed_network_name(pid)
        ),
        None => {
            "services join managed network litebin-<project_id> (Compose networks / network_mode are not applied)"
                .to_string()
        }
    };
    findings.push(finding(
        "litebin.network",
        None,
        FindingDisposition::Translated,
        network_msg,
        None,
    ));

    finalize_report(findings)
}

fn analyze_top_level(
    root: &serde_yaml::Value,
    project_id: Option<&str>,
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
                let msg = match project_id {
                    Some(pid) => format!(
                        "top-level networks are ignored; LiteBin creates {}",
                        managed_network_name(pid)
                    ),
                    None => {
                        "top-level networks are ignored; LiteBin creates a per-project bridge network"
                            .to_string()
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

fn analyze_service(
    svc_name: &str,
    svc: &ComposeService,
    public_service: Option<&str>,
    project_id: Option<&str>,
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
            "ports" => analyze_ports(svc_name, svc, is_public, findings),
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
            findings.push(finding(
                format!("{prefix}.{field}"),
                Some(svc_name.into()),
                *disposition,
                msg,
                None,
            ));
            handled_extra.insert((*field).to_string());
        }
    }

    for (field, message) in UNSUPPORTED_SERVICE_FIELDS {
        if svc.extra.contains_key(*field) {
            let msg = match (*field, project_id) {
                ("network_mode", Some(pid)) => format!(
                    "network_mode is not applied yet; LiteBin always uses managed network {}",
                    managed_network_name(pid)
                ),
                ("networks", Some(pid)) => format!(
                    "custom Compose networks are ignored; LiteBin creates {}",
                    managed_network_name(pid)
                ),
                ("networks", None) => {
                    "custom Compose networks are ignored; LiteBin creates a per-project bridge network"
                        .to_string()
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

fn analyze_ports(
    svc_name: &str,
    svc: &ComposeService,
    is_public: bool,
    findings: &mut Vec<CompatibilityFinding>,
) {
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

fn analyze_volumes(
    svc_name: &str,
    svc: &ComposeService,
    findings: &mut Vec<CompatibilityFinding>,
) {
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
        if is_docker_sock_source(source) {
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
        } else if docker_socket_is_below(source) {
            findings.push(finding(
                format!("{prefix} ({vol})"),
                Some(svc_name.into()),
                FindingDisposition::Unsupported,
                "host bind contains the Docker daemon socket; mount a declared docker.sock path and grant docker-observe instead",
                None,
            ));
        }
    }
}

fn finalize_report(findings: Vec<CompatibilityFinding>) -> Result<CompatibilityReport> {
    let ok = findings
        .iter()
        .all(|f| f.disposition != FindingDisposition::Unsupported);

    let mut caps: BTreeSet<String> = BTreeSet::new();
    for f in &findings {
        if f.disposition == FindingDisposition::PermissionRequired {
            if let Some(ref c) = f.capability {
                caps.insert(c.clone());
            }
        }
    }

    Ok(CompatibilityReport {
        findings,
        ok,
        required_capabilities: caps.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(yaml: &str) -> CompatibilityReport {
        analyze_compose_yaml(yaml, None, None).unwrap().1
    }

    fn report_for(yaml: &str, project_id: &str) -> CompatibilityReport {
        analyze_compose_yaml(yaml, None, Some(project_id)).unwrap().1
    }

    #[test]
    fn simple_web_app_is_ok() {
        let r = report(
            r#"
services:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
"#,
        );
        assert!(r.ok);
        assert!(r.required_capabilities.is_empty());
        assert!(r
            .findings
            .iter()
            .any(|f| f.disposition == FindingDisposition::Translated
                && f.path.contains("ports")));
    }

    #[test]
    fn docker_sock_requires_capability() {
        let r = report(
            r#"
services:
  agent:
    image: example/agent
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
"#,
        );
        assert!(r.ok);
        assert_eq!(r.required_capabilities, vec!["docker-observe".to_string()]);
    }

    #[test]
    fn docker_socket_ancestor_bind_is_unsupported() {
        let r = report(
            r#"
services:
  agent:
    image: example/agent
    volumes:
      - /var:/host-var:ro
"#,
        );
        assert!(!r.ok);
        assert!(r
            .unsupported()
            .any(|finding| finding.message.contains("contains the Docker daemon socket")));
    }

    #[test]
    fn managed_proxy_service_name_is_reserved() {
        let r = report(
            r#"
services:
  litebin-docker-proxy:
    image: attacker/image
"#,
        );
        assert!(!r.ok);
        assert!(r
            .unsupported()
            .any(|finding| finding.message.contains("reserved")));
    }

    #[test]
    fn network_mode_host_is_unsupported() {
        let r = report(
            r#"
services:
  agent:
    image: example/agent
    network_mode: host
"#,
        );
        assert!(!r.ok);
        assert!(r.unsupported().any(|f| f.path.ends_with("network_mode")));
    }

    #[test]
    fn non_public_ports_require_raw_ports() {
        let r = report(
            r#"
services:
  web:
    image: nginx
    ports: ["80:80"]
    labels:
      litebin.public: "true"
  db:
    image: postgres
    ports: ["5432:5432"]
"#,
        );
        assert!(r.ok);
        assert!(r.required_capabilities.contains(&"raw-ports".to_string()));
    }

    #[test]
    fn unknown_service_field_is_unsupported() {
        let r = report(
            r#"
services:
  web:
    image: nginx
    foo_bar: true
"#,
        );
        assert!(!r.ok);
        assert!(r.unsupported().any(|f| f.path.ends_with("foo_bar")));
    }

    #[test]
    fn top_level_networks_overridden() {
        let r = report(
            r#"
networks:
  mynet:
services:
  web:
    image: nginx
"#,
        );
        assert!(r.ok);
        assert!(r.findings.iter().any(|f| {
            f.path == "networks" && f.disposition == FindingDisposition::Overridden
        }));
    }

    #[test]
    fn container_name_is_overridden() {
        let r = report(
            r#"
services:
  web:
    image: nginx
    container_name: myweb
"#,
        );
        assert!(r.ok);
        assert!(r.findings.iter().any(|f| {
            f.path.ends_with("container_name") && f.disposition == FindingDisposition::Overridden
        }));
    }

    #[test]
    fn findings_use_concrete_names_when_project_id_known() {
        let r = report_for(
            r#"
services:
  beszel-agent:
    image: henrygd/beszel-agent
    container_name: agent
    network_mode: host
"#,
            "beszel",
        );
        let container = r
            .findings
            .iter()
            .find(|f| f.path.ends_with("container_name"))
            .expect("container_name finding");
        assert!(container.message.contains("litebin-beszel.beszel-agent"));
        assert!(!container.message.contains('{'));

        let network = r
            .findings
            .iter()
            .find(|f| f.path == "litebin.network")
            .expect("network finding");
        assert!(network.message.contains("litebin-beszel"));
        assert!(!network.message.contains('{'));

        let mode = r
            .unsupported()
            .find(|f| f.path.ends_with("network_mode"))
            .expect("network_mode finding");
        assert!(mode.message.contains("litebin-beszel"));
    }
}
