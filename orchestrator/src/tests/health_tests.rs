use axum::http::StatusCode;

use super::helpers::test_server;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let server = test_server().await;
    server.get("/health").await.assert_status(StatusCode::OK);
}
