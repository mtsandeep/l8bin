mod execute;
mod prepare;
mod rollback;
#[cfg(test)]
mod tests;
mod types;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::AgentState;

use execute::execute_levels;
use prepare::{analyze_and_authorize, cleanup_and_prepare, mutate_plan, persist_project_files};
pub use types::{BatchRunRequest, BatchRunResponse};

/// POST /containers/batch-run
/// Deploy a multi-service project on the agent: store compose, pull images, start in order.
pub async fn batch_run(State(state): State<AgentState>, Json(req): Json<BatchRunRequest>) -> impl IntoResponse {
    tracing::info!(
        project = %req.project_id,
        services = ?req.service_order,
        stage_only = req.stage_only,
        "batch-run request received"
    );

    // Parse and authorize host networking before any filesystem, metadata, network,
    // or container mutation at the agent trust boundary.
    let mut plan = match analyze_and_authorize(&state, &req).await {
        Ok(plan) => plan,
        Err(resp) => return resp,
    };

    // Ensure project directory + metadata + compose.yaml on disk.
    if let Err(resp) = persist_project_files(&state, &req).await {
        return resp;
    }

    // First-deploy staging: prepare runtime files without starting anything.
    if req.stage_only {
        tracing::info!(project = %req.project_id, "batch-run staged (no containers started)");
        return (StatusCode::OK, Json(BatchRunResponse { services: Vec::new(), warnings: Vec::new() })).into_response();
    }

    let mutations = match mutate_plan(&state, &req, &mut plan).await {
        Ok(mutations) => mutations,
        Err(resp) => return resp,
    };

    let removed_services = match cleanup_and_prepare(&state, &req, &plan, &mutations, mutations.proxy_injected).await {
        Ok(removed) => removed,
        Err(resp) => return resp,
    };

    // Build owned lookup: service_name -> RunServiceConfig
    let mut configs_map: std::collections::HashMap<String, litebin_common::types::RunServiceConfig> =
        plan.configs.iter().map(|c| (c.service_name.clone(), c.clone())).collect();

    // Pre-check: warn if a socket declaration has no explicit observation grant.
    let mut warnings: Vec<String> = Vec::new();
    if !mutations.docker_observe {
        let has_sock = plan.configs.iter().any(|c| {
            c.binds.as_ref().is_some_and(|binds| {
                binds.iter().any(|b| {
                    let source = b.split(':').next().unwrap_or("");
                    source.ends_with("/docker.sock")
                })
            })
        });
        if has_sock {
            warnings.push("Docker socket declaration found without docker-observe — the raw socket was removed".into());
        }
    }

    // Start services level by level — parallel within each level
    let results =
        match execute_levels(&state, &req, &plan, &mutations.target_set, &removed_services, &mut configs_map).await {
            Ok(results) => results,
            Err(resp) => return resp,
        };

    // Rebuild local Caddy with all running containers
    if let Err(e) = super::super::waker::rebuild_local_caddy(&state).await {
        tracing::error!(error = %e, "failed to rebuild local Caddy config -- traffic may 502");
    }

    (StatusCode::OK, Json(BatchRunResponse { services: results, warnings })).into_response()
}
