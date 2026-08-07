use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::backend::PasswordBackend;
use crate::nodes;
use crate::routes::manage::{agent_base_url, get_node_from_db};
use litebin_common::upload::{
    self, AGENT_UPLOAD_PREFIX, DEFAULT_TTL_SECS, MINT_PATH, MintRequest, MintResponse, UploadChunkResponse,
    UploadCommitResponse, UploadError, UploadStatusResponse,
};

#[derive(Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct UploadQueryParams {
    pub project_id: String,
    pub image_id: String,
    pub node_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UploadResponse {
    pub image_id: String,
}

#[utoipa::path(
    post,
    path = "/images/upload",
    params(UploadQueryParams),
    responses(
        (status = 200, description = "Image uploaded", body = UploadResponse),
        (status = 401, description = "Authentication required"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "deploy",
    security(("session_auth" = []), ("bearer_token" = [])),
)]
pub async fn upload_image(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<UploadQueryParams>,
    body: Body,
) -> impl IntoResponse {
    // Auth: session first, then deploy token fallback
    let _user_id = match auth_session.user {
        Some(u) => u.id,
        None => match crate::auth::extract_deploy_token(&state, &headers, &params.project_id).await {
            Some(uid) => uid,
            None => {
                return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"})))
                    .into_response();
            }
        },
    };

    let node_id = params.node_id.as_deref().unwrap_or("local");
    let image_id = params.image_id;

    if node_id == "local" {
        // Local path: stream body directly to Docker to load the image
        let byte_stream = body.into_data_stream();
        if let Err(e) = state.docker.load_image(byte_stream).await {
            tracing::error!(error = %e, project = %params.project_id, "failed to load image");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to load image: {e}")})),
            )
                .into_response();
        }
        // Resolve the tag to the actual image ID Docker assigned.
        // OCI format tars may have a different manifest digest than the local config digest,
        // so we inspect by the tag to get the server-side image ID.
        let resolved_id = match state.docker.inspect_image_id(&image_id).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, image_id = %image_id, project = %params.project_id, "image loaded but inspect failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("image loaded but inspect failed: {e}")})),
                )
                    .into_response();
            }
        };
        return (StatusCode::OK, Json(UploadResponse { image_id: resolved_id })).into_response();
    } else {
        // Remote path: stream body to agent via channel bridge
        let resolved_id = match stream_to_agent(&state, node_id, body, &image_id).await {
            Ok(id) => id,
            Err((status, error)) => {
                return (status, Json(serde_json::json!({"error": error}))).into_response();
            }
        };
        return (StatusCode::OK, Json(UploadResponse { image_id: resolved_id })).into_response();
    }
}

async fn stream_to_agent(
    state: &AppState,
    node_id: &str,
    body: Body,
    image_id: &str,
) -> Result<String, (StatusCode, String)> {
    use litebin_common::types::Node;

    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, format!("node '{}' not found", node_id)))?;

    let client = nodes::client::get_node_client(&state.node_clients, node_id)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client not available: {e}")))?;

    let base_url = agent_base_url(&state.config, &node);

    // Stream the body through a channel to avoid buffering the entire image in RAM.
    // axum::Body is !Sync, so we can't wrap it directly in reqwest::Body.
    // Instead, spawn a task that reads chunks from axum Body and sends them
    // through a bounded mpsc channel, then wrap the receiver as a reqwest body.
    let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<axum::body::Bytes, std::io::Error>>(8);

    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let _ = tx.send(chunk.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))).await;
        }
    });

    let streaming_body = reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(rx));

    let resp = client
        .post(format!("{}/images/load?image_id={}", base_url, image_id))
        .header("Content-Type", "application/x-tar")
        .body(streaming_body)
        .send()
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

    if !resp.status().is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent image load failed: {body_text}")));
    }

    // Agent returns the resolved image ID (tag → actual Docker-assigned sha256)
    let agent_resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse agent response: {e}")))?;
    let resolved_id = agent_resp["image_id"].as_str().unwrap_or(image_id).to_string();

    Ok(resolved_id)
}

// =============================================================================
// Chunked, resumable upload (local + relay). Direct-to-agent uploads are minted
// by the agent itself; the broker below just hands the client the agent URL.
// =============================================================================

/// Decide where an image should upload and return a target descriptor. The client
/// then chunks to `{base}/{token}/...`. For local/relay the base is the master
/// (`base_url` absent); for direct it is the agent's public upload URL + CA.
#[derive(Deserialize)]
pub struct UploadTargetRequest {
    pub project_id: String,
    pub image_id: String,
    pub node_id: Option<String>,
    /// `"direct"` or `"relay"`. Defaults to direct when the node supports it.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Serialize)]
pub struct UploadTargetResponse {
    pub mode: String,
    pub token: String,
    pub chunk_size: u64,
    pub expires_at: i64,
    /// Present only for direct uploads (agent public base URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Present only for direct uploads (agent CA PEM for the client to trust).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
}

pub async fn upload_target(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UploadTargetRequest>,
) -> Response {
    // Auth: session first, then deploy token fallback (same as upload_image).
    let _user_id = match auth_session.user {
        Some(u) => u.id,
        None => match crate::auth::extract_deploy_token(&state, &headers, &req.project_id).await {
            Some(uid) => uid,
            None => {
                return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Authentication required"})))
                    .into_response();
            }
        },
    };

    let node_id = req.node_id.clone().unwrap_or_else(|| "local".to_string());

    // Local node: stage on master, load to master Docker at commit.
    if node_id == "local" {
        return match state.upload_store.mint(&req.project_id, &req.image_id, "local", DEFAULT_TTL_SECS) {
            Ok(s) => Json(UploadTargetResponse {
                mode: "local".into(),
                token: s.token,
                chunk_size: state.upload_store.chunk_size(),
                expires_at: s.expires_at,
                base_url: None,
                ca_pem: None,
            })
            .into_response(),
            Err(e) => err_response(e),
        };
    }

    // Remote node.
    let node = match get_node_from_db(&state.db, &node_id).await {
        Ok(n) => n,
        Err((status, msg)) => return (status, Json(serde_json::json!({"error": msg}))).into_response(),
    };

    let want_direct = req.mode.as_deref() != Some("relay");
    let has_public_ip = node.public_ip.as_deref().filter(|s| !s.is_empty()).is_some();
    let has_ca = !state.config.ca_cert_path.is_empty();

    if want_direct && has_public_ip && has_ca {
        // Direct: ask the agent to mint a token.
        let client = match nodes::client::get_node_client(&state.node_clients, &node_id) {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": format!("node client not available: {e}")})),
                )
                    .into_response();
            }
        };
        let base_url = agent_base_url(&state.config, &node);
        let mint_req = MintRequest {
            project_id: req.project_id.clone(),
            image_id: req.image_id.clone(),
            node_id: node_id.clone(),
            ttl_secs: Some(DEFAULT_TTL_SECS),
        };
        let resp = match client
            .post(format!("{base_url}{MINT_PATH}"))
            .json(&mint_req)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": format!("agent unreachable: {e}")})),
                )
                    .into_response();
            }
        };
        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("agent mint failed: {body_text}")})),
            )
                .into_response();
        }
        let mint_resp: MintResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("failed to parse agent mint response: {e}")})),
                )
                    .into_response();
            }
        };
        let ca_pem = std::fs::read_to_string(&state.config.ca_cert_path).ok();
        let public_ip = node.public_ip.as_deref().unwrap_or("");
        return Json(UploadTargetResponse {
            mode: "direct".into(),
            token: mint_resp.token,
            chunk_size: mint_resp.chunk_size,
            expires_at: mint_resp.expires_at,
            base_url: Some(format!("https://{public_ip}{AGENT_UPLOAD_PREFIX}")),
            ca_pem,
        })
        .into_response();
    }

    // Relay: stage on master, stream assembled tar to the agent at commit.
    match state.upload_store.mint(&req.project_id, &req.image_id, &node_id, DEFAULT_TTL_SECS) {
        Ok(s) => Json(UploadTargetResponse {
            mode: "relay".into(),
            token: s.token,
            chunk_size: state.upload_store.chunk_size(),
            expires_at: s.expires_at,
            base_url: None,
            ca_pem: None,
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

/// `GET /images/upload/{token}/status` — which chunks the server already has.
pub async fn chunk_status(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    match state.upload_store.status(&token) {
        Ok((received, total, expires_at)) => Json(UploadStatusResponse {
            received: upload::sorted_indices(received),
            total,
            expires_at,
            chunk_size: state.upload_store.chunk_size(),
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

/// `POST /images/upload/{token}/chunk/{index}` — idempotent chunk write.
pub async fn chunk_upload(
    State(state): State<AppState>,
    Path((token, index)): Path<(String, u64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let total = upload::total_chunks_header(&headers);
    match state.upload_store.write_chunk(&token, index, total, body) {
        Ok((received, total)) => Json(UploadChunkResponse {
            received: upload::sorted_indices(received),
            total,
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

/// `POST /images/upload/{token}/commit` — assemble + load (local) or stream to agent (relay).
pub async fn chunk_commit(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let prep = match state.upload_store.prepare_commit(&token) {
        Ok(p) => p,
        Err(e) => return err_response(e),
    };

    let resolved = if prep.node_id == "local" {
        match upload::load_staged(&state.docker, prep.dir.clone(), prep.total, &prep.image_id).await {
            Ok(id) => id,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("image load failed: {e}")})),
                )
                    .into_response();
            }
        }
    } else {
        match relay_load_to_agent(&state, &prep.node_id, prep.dir.clone(), prep.total, &prep.image_id).await {
            Ok(id) => id,
            Err((status, msg)) => return (status, Json(serde_json::json!({"error": msg}))).into_response(),
        }
    };

    state.upload_store.mark_committed(&token);
    state.upload_store.purge(&token);
    Json(UploadCommitResponse { image_id: resolved }).into_response()
}

/// Concatenate staged chunks and stream them to a remote agent's `/images/load`,
/// returning the resolved image id. Used for relay commits.
async fn relay_load_to_agent(
    state: &AppState,
    node_id: &str,
    dir: std::path::PathBuf,
    total: u64,
    image_id: &str,
) -> Result<String, (StatusCode, String)> {
    use litebin_common::types::Node;

    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("database error: {e}")))?
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, format!("node '{}' not found", node_id)))?;

    let client = nodes::client::get_node_client(&state.node_clients, node_id)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("node client not available: {e}")))?;

    let base_url = agent_base_url(&state.config, &node);

    // Stream assembled chunks straight to the agent (no extra full-file buffering).
    let stream = upload::chunk_stream(dir, total);
    let body = reqwest::Body::wrap_stream(stream);

    let resp = client
        .post(format!("{base_url}/images/load?image_id={image_id}"))
        .header("Content-Type", "application/x-tar")
        .body(body)
        .send()
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("agent unreachable: {e}")))?;

    if !resp.status().is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("agent image load failed: {body_text}")));
    }

    let agent_resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to parse agent response: {e}")))?;
    Ok(agent_resp["image_id"].as_str().unwrap_or(image_id).to_string())
}

fn err_response(e: UploadError) -> Response {
    (e.status(), Json(serde_json::json!({"error": e.to_string()}))).into_response()
}
