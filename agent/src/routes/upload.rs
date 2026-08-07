//! Direct-upload endpoint for the agent (used when the client uploads straight
//! to the agent node rather than relaying through the master).
//!
//! `mint_upload_token` lives on the **main mTLS router** (called by the master).
//! `status` / `chunk` / `commit` live on a **loopback/Docker-network-only** server
//! reached by the agent's Caddy on `:443` via the `/__l8b_upload/*` reverse-proxy
//! route. The token gates access; the management mTLS port is not involved.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use litebin_common::upload::{
    DEFAULT_TTL_SECS, MintRequest, MintResponse, PreparedCommit, UploadChunkResponse, UploadCommitResponse,
    UploadError, UploadStatusResponse,
};

use crate::AgentState;

// --- mint (master → agent, over mTLS) ---------------------------------------

pub async fn mint_upload_token(State(state): State<AgentState>, Json(req): Json<MintRequest>) -> Response {
    let ttl = req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    match state.upload_store.mint(&req.project_id, &req.image_id, &req.node_id, ttl) {
        Ok(session) => Json(MintResponse {
            token: session.token,
            expires_at: session.expires_at,
            chunk_size: state.upload_store.chunk_size(),
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

// --- status / chunk / commit (public via Caddy, token-gated) ----------------

pub async fn upload_status(State(state): State<AgentState>, Path(token): Path<String>) -> Response {
    match state.upload_store.status(&token) {
        Ok((received, total, expires_at)) => Json(UploadStatusResponse {
            received: litebin_common::upload::sorted_indices(received),
            total,
            expires_at,
            chunk_size: state.upload_store.chunk_size(),
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

pub async fn upload_chunk(
    State(state): State<AgentState>,
    Path((token, index)): Path<(String, u64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let total = litebin_common::upload::total_chunks_header(&headers);
    match state.upload_store.write_chunk(&token, index, total, body) {
        Ok((received, total)) => Json(UploadChunkResponse {
            received: litebin_common::upload::sorted_indices(received),
            total,
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

pub async fn commit_upload(State(state): State<AgentState>, Path(token): Path<String>) -> Response {
    let PreparedCommit { dir, total, image_id, .. } = match state.upload_store.prepare_commit(&token) {
        Ok(p) => p,
        Err(e) => return err_response(e),
    };

    // Concatenate staged chunks in order and stream into Docker, then resolve the id.
    let resolved = match litebin_common::upload::load_staged(&state.docker, dir, total, &image_id).await {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("image load failed: {e}") })),
            )
                .into_response();
        }
    };

    state.upload_store.mark_committed(&token);
    state.upload_store.purge(&token);
    Json(UploadCommitResponse { image_id: resolved }).into_response()
}

// --- error mapping ----------------------------------------------------------

fn err_response(e: UploadError) -> Response {
    (e.status(), Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}
