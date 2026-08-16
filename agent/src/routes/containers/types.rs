//! Container lifecycle wire types live in `litebin_common::agent_api` (shared
//! with the orchestrator) and are re-exported here so handler imports stay stable.

pub use litebin_common::agent_api::{
    CleanupRequest, ErrorResponse, LogsQuery, RemoveRequest, RunRequest, RunResponse, StartRequest, StartResponse,
    StopProjectRequest, StopProjectResponse, StopRequest, StopServiceRequest, StopServiceResponse,
};
