//! Split-horizon DNS zone relay: the master pushes this node's zone over the
//! mTLS agent API; the handler ensures the local `litebin-agent-dns` sidecar
//! (same agent binary, `dns` subcommand — so the first push bootstraps it),
//! overrides `answer_ip` with THIS host's bridge gateway (the master cannot
//! know it), and forwards the zone to the sidecar. Zone data and the local
//! push token persist in the agent's data dir — resolution never depends on
//! the master being up. See docs/topics/domains.md.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use litebin_common::dns_server::ZoneSpec;
use litebin_common::docker::{SidecarPortPublish, SidecarSpec};

use crate::AgentState;

pub const SIDECAR_CONTAINER_NAME: &str = "litebin-agent-dns";
pub const SIDECAR_DATA_VOLUME: &str = "litebin-agent-dns-data";
const TOKEN_FILE: &str = "data/dns-push-token";

/// Load (or create on first use) this node's local zone-push token.
fn local_push_token() -> String {
    if let Ok(token) = std::fs::read_to_string(TOKEN_FILE)
        && !token.trim().is_empty()
    {
        return token.trim().to_string();
    }
    let token_bytes: [u8; 24] = rand::random();
    let token = hex::encode(token_bytes);
    if let Some(parent) = std::path::Path::new(TOKEN_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(TOKEN_FILE, &token);
    token
}

/// Desired sidecar state. Tracks the agent's own image (drift = recreate on
/// upgrade) and publishes :53 on this host's default-bridge gateway.
async fn agent_dns_sidecar_spec(state: &AgentState, bind_ip: String, token: &str) -> Option<SidecarSpec> {
    let image = state.docker.self_image_name().await?;
    let image_id = state.docker.self_image_id().await;
    let mut env = vec![
        format!("DNS_PUSH_TOKEN={token}"),
        format!("DNS_BIND_IP={bind_ip}"),
        "DNS_LISTEN=0.0.0.0:53".to_string(),
        "DNS_HTTP_LISTEN=0.0.0.0:5380".to_string(),
        "DNS_ZONE_PATH=/app/data/dns-zone.json".to_string(),
    ];
    if let Ok(upstreams) = std::env::var("UPSTREAM_DNS")
        && !upstreams.trim().is_empty()
    {
        env.push(format!("UPSTREAM_DNS={upstreams}"));
    }
    Some(SidecarSpec {
        name: SIDECAR_CONTAINER_NAME.to_string(),
        image,
        image_id,
        binds: vec![format!("{SIDECAR_DATA_VOLUME}:/app/data")],
        cmd: vec!["/usr/local/bin/litebin-agent".into(), "dns".into()],
        env,
        network: std::env::var("DOCKER_NETWORK")
            .unwrap_or_else(|_| litebin_common::types::DEFAULT_DOCKER_NETWORK.to_string()),
        label: "dns".into(),
        service: "dns".into(),
        ports: vec![
            SidecarPortPublish { host_ip: bind_ip.clone(), host_port: 53, container_port: 53, protocol: "tcp" },
            SidecarPortPublish { host_ip: bind_ip, host_port: 53, container_port: 53, protocol: "udp" },
        ],
        memory_limit: Some(64 * 1024 * 1024),
        memory_reservation: Some(12 * 1024 * 1024),
    })
}

pub async fn push_dns_zone(
    State(state): State<AgentState>,
    mut zone: Json<ZoneSpec>,
) -> Result<StatusCode, (StatusCode, String)> {
    if std::env::var("L8B_DISABLE_SPLIT_DNS").is_ok_and(|v| v == "1") {
        return Err((StatusCode::FORBIDDEN, "split DNS disabled on this node (L8B_DISABLE_SPLIT_DNS)".into()));
    }

    let token = local_push_token();
    let bind_ip = match std::env::var("DNS_BIND_IP") {
        Ok(ip) if !ip.is_empty() => ip,
        _ => state.docker.default_bridge_gateway().await.unwrap_or_else(|| "172.17.0.1".into()),
    };
    // The zone is host-local: this node's sidecar must answer with this
    // node's own gateway, not whatever the master sent.
    zone.answer_ip = bind_ip.clone();

    let spec = agent_dns_sidecar_spec(&state, bind_ip.clone(), &token)
        .await
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "agent must run in a container for split DNS".into()))?;
    state
        .docker
        .ensure_sidecar_container(&spec)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("failed to ensure dns sidecar: {e}")))?;

    let base = std::env::var("DNS_SIDECAR_URL").unwrap_or_else(|_| format!("http://{SIDECAR_CONTAINER_NAME}:5380"));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let response = client
        .post(format!("{base}/load"))
        .bearer_auth(&token)
        .json(&*zone)
        .send()
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("local dns sidecar unreachable: {e}")))?;
    let status = response.status();
    if status.is_success() {
        tracing::info!(status = %status, "relayed dns zone to local sidecar");
        ensure_dns_injection_monitor(&state, &bind_ip);
        Ok(status)
    } else {
        Err((StatusCode::BAD_GATEWAY, format!("local dns sidecar /load returned {status}")))
    }
}

/// One-shot activation of the per-container DNS injection monitor (idempotent).
/// Gates injection on the local sidecar's health; without it the agent never
/// bakes a dead resolver into new containers.
fn ensure_dns_injection_monitor(state: &AgentState, bind_ip: &str) {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return; // already running
    }
    let docker = Arc::clone(&state.docker);
    let base = std::env::var("DNS_SIDECAR_URL").unwrap_or_else(|_| format!("http://{SIDECAR_CONTAINER_NAME}:5380"));
    let bind_ip = bind_ip.to_string();
    tokio::spawn(async move {
        let client =
            reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)).build().expect("reqwest client");
        let upstreams: Vec<String> = std::env::var("UPSTREAM_DNS")
            .map(|raw| raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        let mut configured = false;
        let mut last_healthy = None;
        loop {
            let exists = docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await.ok().flatten().is_some();
            if exists {
                if !configured {
                    let mut servers = vec![bind_ip.clone()];
                    servers.extend(upstreams.iter().cloned());
                    docker.set_dns_servers(Some(servers));
                    configured = true;
                }
                let healthy = client.get(format!("{base}/health")).send().await.is_ok_and(|r| r.status().is_success());
                if last_healthy != Some(healthy) {
                    tracing::info!(healthy, "agent split-dns sidecar health changed");
                    last_healthy = Some(healthy);
                }
                docker.set_split_dns_healthy(healthy);
            } else {
                docker.set_split_dns_healthy(false);
                configured = false;
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
