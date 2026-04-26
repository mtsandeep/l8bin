use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use crate::auth::backend::PasswordBackend;
use crate::nodes;
use crate::routes::manage::agent_base_url;
use crate::AppState;

#[derive(Deserialize)]
pub struct UploadQueryParams {
    pub project_id: String,
    pub image_id: String,
    pub node_id: Option<String>,
}

#[derive(Serialize)]
pub struct UploadResponse {
    pub image_id: String,
}

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
        None => {
            match crate::auth::extract_deploy_token(&state, &headers, &params.project_id).await {
                Some(uid) => uid,
                None => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error": "Authentication required"})),
                    )
                        .into_response();
                }
            }
        }
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
            ).into_response();
        }
        // Verify the image actually exists in Docker after loading
        match state.docker.inspect_image(&image_id).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, image_id = %image_id, project = %params.project_id, "image load reported success but image not found in Docker");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("image loaded but not found in Docker: {e}")})),
                ).into_response();
            }
        }
    } else {
        // Remote path: stream body to agent via channel bridge
        if let Err((status, error)) = stream_to_agent(&state, node_id, body, &image_id).await {
            return (status, Json(serde_json::json!({"error": error}))).into_response();
        }
    }

    (StatusCode::OK, Json(UploadResponse { image_id })).into_response()
}

async fn stream_to_agent(
    state: &AppState,
    node_id: &str,
    body: Body,
    image_id: &str,
) -> Result<(), (StatusCode, String)> {
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

    Ok(())
}
