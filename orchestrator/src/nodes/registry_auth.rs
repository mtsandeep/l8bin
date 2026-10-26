//! Registry pull config for agent dispatches.
//!
//! Agents pull images themselves, so the orchestrator attaches the registry
//! configuration (default registry, credentials, mirror chains) to
//! run/batch-run requests. Nodes are assumed to run the same litebin version
//! as the orchestrator; an out-of-date agent that ignores the field fails its
//! private-registry pull until upgraded.

use litebin_common::registry::RegistryWireConfig;

/// Full registry pull config for dispatches to `node_id`. Local pulls use the
/// orchestrator's own DockerManager state, not this path.
pub async fn registry_config_for_node(
    db: &sqlx::SqlitePool,
    node_id: &str,
) -> anyhow::Result<Option<RegistryWireConfig>> {
    if node_id == "local" {
        return Ok(None);
    }
    Ok(Some(crate::routes::registries::load_registry_wire_config(db).await?))
}
