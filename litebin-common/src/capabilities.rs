//! Project capability registry and helpers.
//!
//! Capabilities are explicit grants stored per project. Compose files may
//! *request* capabilities; only the user can *grant* them.

use serde::{Deserialize, Serialize};

/// Stable capability identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectCapability {
    /// Observe the host Docker daemon through LiteBin's read-only proxy.
    #[serde(rename = "docker-observe")]
    DockerObserve,
    /// Run approved background services in the host network namespace.
    #[serde(rename = "host-network")]
    HostNetwork,
    /// Publish Compose-declared ports directly on the host (bypass Caddy).
    #[serde(rename = "raw-ports")]
    RawPorts,
}

impl ProjectCapability {
    pub const ALL: &'static [ProjectCapability] =
        &[ProjectCapability::DockerObserve, ProjectCapability::HostNetwork, ProjectCapability::RawPorts];

    pub fn id(self) -> &'static str {
        match self {
            ProjectCapability::DockerObserve => "docker-observe",
            ProjectCapability::HostNetwork => "host-network",
            ProjectCapability::RawPorts => "raw-ports",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProjectCapability::DockerObserve => "Docker observation",
            ProjectCapability::HostNetwork => "Host networking",
            ProjectCapability::RawPorts => "Raw host ports",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ProjectCapability::DockerObserve => {
                "Allows selected services to read host-wide container information through LiteBin's \
                 endpoint-allowlisted proxy. The raw Docker socket is never mounted into the workload."
            }
            ProjectCapability::HostNetwork => {
                "Runs approved background services in the host network namespace. \
                 The service can access host-bound listeners and exposes its own listeners directly."
            }
            ProjectCapability::RawPorts => {
                "Publishes Compose-declared container ports directly on the host (0.0.0.0). \
                 Required for non-HTTP services (databases, games, UDP) that need host bindings."
            }
        }
    }

    pub fn risk(self) -> &'static str {
        match self {
            ProjectCapability::DockerObserve => {
                "High — responses may expose host-wide container metadata, environment values, and logs."
            }
            ProjectCapability::HostNetwork => {
                "High — removes network namespace isolation and may expose host services or create listener conflicts."
            }
            ProjectCapability::RawPorts => {
                "Medium — opens host ports and can conflict with other services; LiteBin-reserved ports are still blocked."
            }
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "docker-observe" => Some(ProjectCapability::DockerObserve),
            "host-network" => Some(ProjectCapability::HostNetwork),
            "raw-ports" => Some(ProjectCapability::RawPorts),
            _ => None,
        }
    }

    /// True when changing this grant requires recreating containers to take effect.
    pub fn requires_recreate(self) -> bool {
        true
    }
}

impl std::fmt::Display for ProjectCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Catalog entry returned by the API / dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CapabilityInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub risk: String,
    pub requires_recreate: bool,
}

impl From<ProjectCapability> for CapabilityInfo {
    fn from(c: ProjectCapability) -> Self {
        Self {
            id: c.id().to_string(),
            label: c.label().to_string(),
            description: c.description().to_string(),
            risk: c.risk().to_string(),
            requires_recreate: c.requires_recreate(),
        }
    }
}

/// Granted capability row for a project.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ProjectCapabilityGrant {
    pub project_id: String,
    pub capability: String,
    pub granted_at: i64,
    pub granted_by: Option<String>,
}

/// Full capability status for a project (catalog + grant state).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectCapabilityStatus {
    #[serde(flatten)]
    pub info: CapabilityInfo,
    pub granted: bool,
    pub granted_at: Option<i64>,
    /// Why the current Compose file requests this capability, if known.
    pub requested_reason: Option<String>,
}

/// Return the full static capability catalog.
pub fn capability_catalog() -> Vec<CapabilityInfo> {
    ProjectCapability::ALL.iter().copied().map(CapabilityInfo::from).collect()
}

/// Parse and dedupe capability ids from a list; unknown ids are skipped with `None` returned for that slot.
pub fn parse_capability_ids(ids: &[String]) -> Result<Vec<ProjectCapability>, String> {
    let mut out = Vec::new();
    for id in ids {
        match ProjectCapability::parse(id) {
            Some(c) if !out.contains(&c) => out.push(c),
            Some(_) => {}
            None => return Err(format!("unknown capability '{id}'")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_exposes_observation_but_not_legacy_mutating_access() {
        let ids: Vec<String> = capability_catalog().into_iter().map(|item| item.id).collect();
        assert!(ids.contains(&"docker-observe".to_string()));
        assert!(ids.contains(&"host-network".to_string()));
        assert!(!ids.contains(&"docker-access".to_string()));
    }
}
