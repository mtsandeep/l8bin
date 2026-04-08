use axum::{
    body::Body,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::AgentState;

#[derive(Serialize)]
pub struct LoadImageResponse {
    pub image_id: String,
}

#[derive(Deserialize)]
pub struct RemoveImageRequest {
    pub image: String,
}

#[derive(Serialize)]
pub struct RemoveImageResponse {
    pub removed: bool,
}

#[derive(Serialize)]
pub struct PruneResponse {
    pub bytes_reclaimed: u64,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /images/load
/// Accepts a raw tar body (docker save output) and loads it into Docker.
/// Streams the body directly to Docker without buffering.
pub async fn load_image(
    State(state): State<AgentState>,
    body: Body,
) -> impl IntoResponse {
    let byte_stream = body.into_data_stream();

    match state.docker.load_image(byte_stream).await {
        Ok(image_id) => (StatusCode::OK, Json(LoadImageResponse { image_id })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e.to_string() }),
            )
                .into_response()
        }
    }
}

/// POST /images/remove-unused
/// Removes an image if it's not actively used by any container.
pub async fn remove_unused_image(
    State(state): State<AgentState>,
    Json(req): Json<RemoveImageRequest>,
) -> impl IntoResponse {
    match state.docker.remove_unused_image(&req.image).await {
        Ok(removed) => (StatusCode::OK, Json(RemoveImageResponse { removed })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to remove unused image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e.to_string() }),
            )
                .into_response()
        }
    }
}

/// POST /images/prune
/// Prunes all dangling (unused) images and returns bytes reclaimed.
pub async fn prune_images(
    State(state): State<AgentState>,
) -> impl IntoResponse {
    match state.docker.prune_dangling_images().await {
        Ok(reclaimed) => (
            StatusCode::OK,
            Json(PruneResponse { bytes_reclaimed: reclaimed }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to prune images");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e.to_string() }),
            )
                .into_response()
        }
    }
}
