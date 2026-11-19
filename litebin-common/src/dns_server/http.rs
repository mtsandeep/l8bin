//! Zone-push HTTP listener for the dns sidecar: `GET /health` (monitoring)
//! and token-gated `POST /load` (full-zone snapshot swap). Never published
//! outside the host's `litebin-network`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::routing::{get, post};
use axum::{Json, Router};
use sha2::{Digest, Sha256};

use super::SharedZone;

pub struct HttpState {
    pub zone: SharedZone,
    pub push_token: Option<String>,
}

pub async fn serve(listen: std::net::SocketAddr, zone: SharedZone, push_token: Option<String>) -> anyhow::Result<()> {
    let state = Arc::new(HttpState { zone, push_token });
    let app = Router::new().route("/health", get(health)).route("/load", post(load)).with_state(state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "dns zone-push http listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<Arc<HttpState>>) -> Json<serde_json::Value> {
    let zone = state.zone.read().expect("zone lock");
    Json(serde_json::json!({
        "zone_sha256": zone.hash_hex(),
        "answer_ip": zone.answer_ip,
        "wildcard_domains": zone.wildcard_domains.len(),
        "exact_names": zone.exact_names.len(),
    }))
}

async fn load(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    let Some(expected) = state.push_token.as_deref() else {
        return Err((StatusCode::FORBIDDEN, "zone push disabled (no token configured)"));
    };
    let presented = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    // Constant-time-ish compare: hash both sides before eq.
    if Sha256::digest(presented.as_bytes()) != Sha256::digest(expected.as_bytes()) {
        return Err((StatusCode::UNAUTHORIZED, "invalid token"));
    }
    let incoming: super::ZoneSpec =
        serde_json::from_slice(&body).map_err(|_| (StatusCode::BAD_REQUEST, "invalid zone JSON"))?;
    let mut zone = state.zone.write().expect("zone lock");
    if zone.hash_hex() == incoming.hash_hex() {
        return Ok(StatusCode::NO_CONTENT);
    }
    *zone = incoming;
    tracing::info!(zone_sha256 = %zone.hash_hex(), "dns zone updated");
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_server::ZoneSpec;
    use std::sync::RwLock;

    #[tokio::test]
    async fn load_requires_token_and_dedupes_unchanged_zones() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode as Status};
        use tower::util::ServiceExt;

        let state =
            Arc::new(HttpState { zone: Arc::new(RwLock::new(ZoneSpec::default())), push_token: Some("secret".into()) });
        let app = Router::new().route("/load", post(load)).with_state(Arc::clone(&state));

        let request = |body: &str, token: bool| {
            let mut builder = Request::builder().method("POST").uri("/load");
            if token {
                builder = builder.header(AUTHORIZATION, "Bearer secret");
            }
            builder.body(Body::from(body.to_string())).unwrap()
        };

        let no_auth = app.clone().oneshot(request("", false)).await.unwrap();
        assert_eq!(no_auth.status(), Status::UNAUTHORIZED);

        let zone = ZoneSpec {
            answer_ip: "172.17.0.1".into(),
            ttl_secs: 5,
            wildcard_domains: vec!["litebin.dev".into()],
            exact_names: vec![],
        };
        let first = app.clone().oneshot(request(&serde_json::to_string(&zone).unwrap(), true)).await.unwrap();
        assert_eq!(first.status(), Status::OK);
        let second = app.clone().oneshot(request(&serde_json::to_string(&zone).unwrap(), true)).await.unwrap();
        assert_eq!(second.status(), Status::NO_CONTENT);
        let bad = app.oneshot(request("{\"answer_ip\": 1}", true)).await.unwrap();
        assert_eq!(bad.status(), Status::BAD_REQUEST);
    }
}
