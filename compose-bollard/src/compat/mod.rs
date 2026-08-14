mod helpers;
mod service;
mod tables;
mod top_level;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ComposeParser;
use crate::error::{ComposeError, Result};
use crate::parse::{ComposeFile, ComposeService};
use helpers::{finding, managed_network_name};
use service::analyze_service;
use top_level::analyze_top_level;

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
        self.findings.iter().filter(|f| f.disposition == FindingDisposition::Unsupported)
    }

    pub fn permission_required(&self) -> impl Iterator<Item = &CompatibilityFinding> {
        self.findings.iter().filter(|f| f.disposition == FindingDisposition::PermissionRequired)
    }
}

/// Analyze a Compose YAML string for LiteBin compatibility.
///
/// `public_service` is the service LiteBin will treat as the HTTP ingress target
/// (label / port / CLI selection). Pass `None` to use automatic detection.
///
/// When `project_id` is provided, findings use concrete names
/// (e.g. `litebin-monitor.host-agent`) instead of placeholders.
pub fn analyze_compose_yaml(
    yaml: &str,
    public_service: Option<&str>,
    project_id: Option<&str>,
) -> Result<(ComposeFile, CompatibilityReport)> {
    analyze_compose_yaml_for_workload(yaml, public_service, project_id, false)
}

pub fn analyze_compose_yaml_for_workload(
    yaml: &str,
    public_service: Option<&str>,
    project_id: Option<&str>,
    is_background: bool,
) -> Result<(ComposeFile, CompatibilityReport)> {
    let root: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    let compose = ComposeParser::parse(yaml)?;
    let report = analyze_compose_for_workload(&root, &compose, public_service, project_id, is_background)?;
    Ok((compose, report))
}

/// Analyze an already-parsed Compose file plus its raw YAML root.
pub fn analyze_compose(
    root: &serde_yaml::Value,
    compose: &ComposeFile,
    public_service: Option<&str>,
    project_id: Option<&str>,
) -> Result<CompatibilityReport> {
    analyze_compose_for_workload(root, compose, public_service, project_id, false)
}

pub fn analyze_compose_for_workload(
    root: &serde_yaml::Value,
    compose: &ComposeFile,
    public_service: Option<&str>,
    project_id: Option<&str>,
    is_background: bool,
) -> Result<CompatibilityReport> {
    let mut findings = Vec::new();

    let has_host_network = compose.services.values().any(ComposeService::uses_host_network);
    analyze_top_level(root, project_id, has_host_network, &mut findings);

    if compose.services.is_empty() {
        return Err(ComposeError::NoServices);
    }

    // Validate graph early so callers get a clear error, but still build findings.
    let _ = compose.topological_sort()?;

    let detected_public = if is_background {
        None
    } else {
        match public_service {
            Some(name) => {
                if !compose.services.contains_key(name) {
                    return Err(ComposeError::ServiceNotFound { name: name.to_string() });
                }
                Some(name.to_string())
            }
            None => compose.detect_public_service()?,
        }
    };

    if let Some(ref pub_svc) = detected_public {
        findings.push(finding(
            format!("services.{pub_svc}"),
            Some(pub_svc.clone()),
            FindingDisposition::Translated,
            format!("'{pub_svc}' is the public HTTP service; LiteBin will route {pub_svc}.{{domain}} to it"),
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
        analyze_service(svc_name, svc, detected_public.as_deref(), project_id, is_background, &mut findings);
    }

    // Always note LiteBin security overrides once (project-level).
    findings.push(finding(
        "litebin.security",
        None,
        FindingDisposition::Overridden,
        "LiteBin applies capability drop/add, no-new-privileges, pids_limit, and log rotation to all services",
        None,
    ));
    let network_msg = if has_host_network {
        "host-network services use the host namespace; other services use LiteBin's managed project bridge".to_string()
    } else {
        match project_id {
            Some(pid) => format!(
                "services join managed network {} (Compose networks / network_mode are not applied)",
                managed_network_name(pid)
            ),
            None => {
                "services join managed network litebin-<project_id> (Compose networks / network_mode are not applied)"
                    .to_string()
            }
        }
    };
    findings.push(finding("litebin.network", None, FindingDisposition::Translated, network_msg, None));

    finalize_report(findings)
}

fn finalize_report(findings: Vec<CompatibilityFinding>) -> Result<CompatibilityReport> {
    let ok = findings.iter().all(|f| f.disposition != FindingDisposition::Unsupported);

    let mut caps: BTreeSet<String> = BTreeSet::new();
    for f in &findings {
        if f.disposition == FindingDisposition::PermissionRequired {
            if let Some(ref c) = f.capability {
                caps.insert(c.clone());
            }
        }
    }

    Ok(CompatibilityReport { findings, ok, required_capabilities: caps.into_iter().collect() })
}
