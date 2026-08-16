//! Batch-run wire types live in `litebin_common::agent_api` (shared with the
//! orchestrator) and are re-exported here so handler imports stay stable.

pub use litebin_common::agent_api::{BatchRunErrorResponse, BatchRunRequest, BatchRunResponse, ServiceRunResult};

pub(super) fn host_network_authorized(granted: bool, is_background: bool) -> bool {
    granted && is_background
}
