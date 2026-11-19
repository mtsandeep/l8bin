//! Split-horizon DNS sidecar control: the `litebin-dns` sidecar (same
//! orchestrator image, `dns` subcommand, published on the default-bridge
//! gateway) behind the `split_dns_enabled` setting. Builds the internal zone,
//! ensures/removes the sidecar, gates per-container DNS injection on sidecar
//! health, and pushes per-node zones to agents.
//! See docs/topics/domains.md.

use std::collections::HashMap;

use litebin_common::dns_server::ZoneSpec;
use litebin_common::docker::{SidecarPortPublish, SidecarSpec};
use litebin_common::types::RoutingMode;

use crate::platform::PlatformSettings;

pub const SIDECAR_CONTAINER_NAME: &str = "litebin-dns";
pub const SIDECAR_DATA_VOLUME: &str = "litebin-dns-data";

/// Live split-DNS state surfaced to the Dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SplitDnsStatus {
    pub enabled: bool,
    /// Sidecar reachable and answering /health.
    pub healthy: bool,
    /// Bridge-IP the resolver is published on.
    pub bind_ip: String,
    pub zone_sha256: Option<String>,
    /// Last ensure/health error (port conflicts, image gone, …).
    pub error: Option<String>,
}

/// HTTP client for the local `litebin-dns` sidecar (`/load` + `/health`).
pub struct DnsSidecarClient {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl DnsSidecarClient {
    pub fn new(base: String, token: String) -> Self {
        Self {
            base,
            token,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn health(&self) -> Option<serde_json::Value> {
        let response = self.client.get(format!("{}/health", self.base)).send().await.ok()?;
        response.json().await.ok()
    }

    /// Full-snapshot push; sidecar answers 204 when unchanged.
    pub async fn push_zone(&self, zone: &ZoneSpec) -> anyhow::Result<()> {
        let status =
            self.client.post(format!("{}/load", self.base)).bearer_auth(&self.token).json(zone).send().await?.status();
        if status.is_success() { Ok(()) } else { anyhow::bail!("dns sidecar /load returned {status}") }
    }
}

/// Enabled flag + shared token + bridge bind IP from the settings table,
/// generating/persisting the latter two on first enable.
pub async fn split_dns_config(
    db: &sqlx::SqlitePool,
    docker: &litebin_common::docker::DockerManager,
) -> (bool, String, String) {
    let enabled = crate::routes::global_settings::get_setting(db, "split_dns_enabled")
        .await
        .ok()
        .flatten()
        .is_some_and(|v| v == "1");

    let token = match crate::routes::global_settings::get_setting(db, "split_dns_token").await.ok().flatten() {
        Some(token) if !token.is_empty() => token,
        _ => {
            let token_bytes: [u8; 24] = rand::random();
            let token = hex::encode(token_bytes);
            let _ = crate::routes::global_settings::upsert_setting(db, "split_dns_token", &token).await;
            token
        }
    };

    let bind_ip = match crate::routes::global_settings::get_setting(db, "split_dns_bind_ip").await.ok().flatten() {
        Some(ip) if !ip.is_empty() => ip,
        _ => {
            let detected = docker.default_bridge_gateway().await.unwrap_or_else(|| "172.17.0.1".into());
            let _ = crate::routes::global_settings::upsert_setting(db, "split_dns_bind_ip", &detected).await;
            detected
        }
    };
    (enabled, token, bind_ip)
}

/// Desired state for the dns sidecar. `image`/`image_id` come from the
/// orchestrator's self-inspection, so the sidecar always tracks the running
/// platform binary (drift = recreate on upgrade).
#[allow(clippy::too_many_arguments)]
pub fn dns_sidecar_spec(
    image: &str,
    image_id: Option<String>,
    network: &str,
    bind_ip: &str,
    token: &str,
    upstreams: &[String],
) -> SidecarSpec {
    let mut env = vec![
        format!("DNS_PUSH_TOKEN={token}"),
        format!("DNS_BIND_IP={bind_ip}"),
        "DNS_LISTEN=0.0.0.0:53".to_string(),
        "DNS_HTTP_LISTEN=0.0.0.0:5380".to_string(),
        "DNS_ZONE_PATH=/app/data/dns-zone.json".to_string(),
    ];
    if !upstreams.is_empty() {
        env.push(format!("UPSTREAM_DNS={}", upstreams.join(",")));
    }
    SidecarSpec {
        name: SIDECAR_CONTAINER_NAME.to_string(),
        image: image.to_string(),
        image_id,
        binds: vec![format!("{SIDECAR_DATA_VOLUME}:/app/data")],
        cmd: vec!["/app/litebin-orchestrator".into(), "dns".into()],
        env,
        network: network.to_string(),
        label: "dns".into(),
        service: "dns".into(),
        ports: vec![
            SidecarPortPublish { host_ip: bind_ip.to_string(), host_port: 53, container_port: 53, protocol: "tcp" },
            SidecarPortPublish { host_ip: bind_ip.to_string(), host_port: 53, container_port: 53, protocol: "udp" },
        ],
        memory_limit: Some(64 * 1024 * 1024),
        memory_reservation: Some(12 * 1024 * 1024),
    }
}

/// The public IP platform DNS points at (dns_target override wins — mirrors
/// `resolved_dns_ip` in cloudflare_router/dns.rs).
fn resolved_platform_ip(snap: &PlatformSettings, config_public_ip: &str) -> String {
    if snap.dns_target.trim().is_empty() { config_public_ip.to_string() } else { snap.dns_target.trim().to_string() }
}

/// Does `name` publicly resolve to `ip`? (CDN/WAF rule: a custom domain
/// enters the internal zone only when its public record points at this host —
/// CDN-fronted domains resolve to edge IPs and must stay external.)
async fn resolves_to(name: &str, ip: &str) -> bool {
    match tokio::net::lookup_host((name, 0)).await {
        Ok(addrs) => addrs.into_iter().any(|a| a.ip().to_string() == ip),
        Err(_) => false,
    }
}

/// Non-background custom domains (+ their www pairs).
async fn custom_domain_candidates(db: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT custom_domain FROM projects
         WHERE is_background = 0 AND custom_domain IS NOT NULL AND custom_domain != ''",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .flat_map(|cd| {
        let www = format!("www.{cd}");
        [cd, www]
    })
    .collect()
}

/// Build the internal zone served to containers on THIS host.
///
/// - `master_proxy`: wildcard the platform domain (+ previous domain during
///   its grace window) — every platform name terminates on this host's Caddy;
///   custom domains are enumerated under the resolves-to-this-host rule.
/// - `cloudflare_dns`: exact names only, restricted to records pointing at
///   this host — a name hosted on another node must resolve real or the local
///   Caddy would black-hole it.
pub async fn desired_internal_zone(
    db: &sqlx::SqlitePool,
    snap: &PlatformSettings,
    config_public_ip: &str,
    routing_mode: &RoutingMode,
    gateway_ip: &str,
) -> ZoneSpec {
    let host_ip = resolved_platform_ip(snap, config_public_ip);
    let mut zone = ZoneSpec { answer_ip: gateway_ip.to_string(), ..Default::default() };

    match routing_mode {
        RoutingMode::MasterProxy => {
            if !snap.domain.is_empty() {
                zone.wildcard_domains.push(snap.domain.clone());
            }
            if !snap.previous_domain.is_empty() {
                zone.wildcard_domains.push(snap.previous_domain.clone());
            }
        }
        RoutingMode::CloudflareDns => {
            let desired: HashMap<String, String> = crate::cloudflare_router::dns::desired_dns_records(
                db,
                &host_ip,
                &snap.domain,
                &snap.dashboard_subdomain,
                &snap.poke_subdomain,
            )
            .await;
            zone.exact_names = desired.into_iter().filter(|(_, ip)| ip == &host_ip).map(|(name, _)| name).collect();
        }
    }

    // Custom domains (all modes): same-host public resolution required.
    for cd in custom_domain_candidates(db).await {
        if !zone.exact_names.iter().any(|n| n.eq_ignore_ascii_case(&cd)) && resolves_to(&cd, &host_ip).await {
            zone.exact_names.push(cd);
        }
    }
    zone
}

/// Per-node internal zone (cloudflare_dns mode only): exact names whose
/// public record points at that node — a name hosted elsewhere must resolve
/// real or the node's local Caddy would black-hole it. `answer_ip` is
/// overridden by the agent relay with the node's own bridge gateway.
async fn desired_node_zone(
    db: &sqlx::SqlitePool,
    snap: &PlatformSettings,
    config_public_ip: &str,
    node_ip: &str,
) -> ZoneSpec {
    let host_ip = resolved_platform_ip(snap, config_public_ip);
    let desired = crate::cloudflare_router::dns::desired_dns_records(
        db,
        &host_ip,
        &snap.domain,
        &snap.dashboard_subdomain,
        &snap.poke_subdomain,
    )
    .await;
    let mut zone = ZoneSpec {
        exact_names: desired.into_iter().filter(|(_, ip)| ip == node_ip).map(|(name, _)| name).collect(),
        ..Default::default()
    };
    for cd in custom_domain_candidates(db).await {
        if !zone.exact_names.iter().any(|n| n.eq_ignore_ascii_case(&cd)) && resolves_to(&cd, node_ip).await {
            zone.exact_names.push(cd);
        }
    }
    zone
}

/// Periodic reconcile (janitor-style): ensure/remove the sidecar per the
/// Dashboard setting, push the master's zone (204-deduped, so every tick is
/// cheap), push per-node zones in cloudflare_dns mode, and keep the
/// CREATE-time injection gate aligned with sidecar health — a dead sidecar is
/// never baked into new containers. Toggles notify for immediate response.
pub async fn run_sidecar_reconcile(state: std::sync::Arc<crate::AppState>) {
    let base = std::env::var("DNS_SIDECAR_URL").unwrap_or_else(|_| "http://litebin-dns:5380".into());
    let upstreams: Vec<String> = std::env::var("UPSTREAM_DNS")
        .map(|raw| raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let mut last_healthy = None;

    loop {
        let (enabled, token, bind_ip) = split_dns_config(&state.db, &state.docker).await;
        let mut status =
            SplitDnsStatus { enabled, healthy: false, bind_ip: bind_ip.clone(), zone_sha256: None, error: None };

        if !enabled {
            // Teardown is idempotent; injection stops immediately.
            if state.docker.find_container_by_name(SIDECAR_CONTAINER_NAME).await.ok().flatten().is_some()
                && let Err(e) = state.docker.remove_sidecar_container(SIDECAR_CONTAINER_NAME).await
            {
                status.error = Some(e.to_string());
            }
            state.docker.set_dns_servers(None);
            state.docker.set_split_dns_healthy(false);
            *state.split_dns_status.write().expect("split dns status lock") = status;
            reconcile_wait(&state.split_dns_notify).await;
            continue;
        }

        // Self-image: only available when running inside a container (dev on
        // the host can't host the publish anyway).
        let Some(image) = state.docker.self_image_name().await else {
            status.error = Some("split DNS requires the orchestrator to run in a container".into());
            state.docker.set_split_dns_healthy(false);
            *state.split_dns_status.write().expect("split dns status lock") = status;
            reconcile_wait(&state.split_dns_notify).await;
            continue;
        };
        let image_id = state.docker.self_image_id().await;

        let rootless = state.docker.host_info().await.ok().and_then(|i| i.rootless);
        if rootless == Some(true) {
            status.error = Some("rootless Docker cannot host the bridge-IP DNS publish".into());
            state.docker.set_split_dns_healthy(false);
            *state.split_dns_status.write().expect("split dns status lock") = status;
            reconcile_wait(&state.split_dns_notify).await;
            continue;
        }

        let spec = dns_sidecar_spec(&image, image_id, &state.config.docker_network, &bind_ip, &token, &upstreams);
        if let Err(e) = state.docker.ensure_sidecar_container(&spec).await {
            tracing::warn!(error = %e, "failed to ensure dns sidecar");
            status.error = Some(e.to_string());
            state.docker.set_split_dns_healthy(false);
            *state.split_dns_status.write().expect("split dns status lock") = status;
            reconcile_wait(&state.split_dns_notify).await;
            continue;
        }

        let client = DnsSidecarClient::new(base.clone(), token.clone());
        let health = client.health().await;
        let healthy = health.is_some();
        if last_healthy != Some(healthy) {
            tracing::info!(healthy, "split-dns sidecar health changed");
            last_healthy = Some(healthy);
        }
        state.docker.set_split_dns_healthy(healthy);
        status.healthy = healthy;

        if healthy {
            status.zone_sha256 =
                health.as_ref().and_then(|h| h.get("zone_sha256")).and_then(|v| v.as_str()).map(str::to_string);

            // Inject per-container DNS: gateway first, upstreams as fallback.
            let mut servers = vec![bind_ip.clone()];
            servers.extend(upstreams.iter().cloned());
            state.docker.set_dns_servers(Some(servers));

            let snap = state.platform.snapshot();
            let mode = state.router.read().await.routing_mode();
            let zone = desired_internal_zone(&state.db, &snap, &state.config.public_ip, &mode, &bind_ip).await;
            if let Err(e) = client.push_zone(&zone).await {
                tracing::warn!(error = %e, "failed to push internal dns zone");
                status.error = Some(e.to_string());
            }

            // cloudflare_dns: each online agent gets its own exact-name zone
            // (the relay also ensures the agent's sidecar on first push).
            if matches!(mode, RoutingMode::CloudflareDns) {
                let nodes = match sqlx::query_as::<_, (String, String)>(
                    "SELECT id, public_ip FROM nodes WHERE id != 'local' AND status = 'online'",
                )
                .fetch_all(&state.db)
                .await
                {
                    Ok(rows) => rows.into_iter().filter(|(_, ip)| !ip.is_empty()).collect::<Vec<_>>(),
                    Err(e) => {
                        tracing::warn!(error = %e, "dns zone reconcile: failed to fetch nodes");
                        Vec::new()
                    }
                };
                for (node_id, node_ip) in nodes {
                    let zone = desired_node_zone(&state.db, &snap, &state.config.public_ip, &node_ip).await;
                    match crate::nodes::client::AgentClient::resolve(&state, &node_id).await {
                        Ok(agent) => {
                            if let Err(e) = agent.push_dns_zone(&zone).await {
                                tracing::warn!(node = %node_id, error = %e, "failed to push dns zone to agent");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(node = %node_id, error = %e, "dns zone push: agent client unavailable")
                        }
                    }
                }
            }
        }

        *state.split_dns_status.write().expect("split dns status lock") = status;
        reconcile_wait(&state.split_dns_notify).await;
    }
}

/// 30 s tick, or immediate when a toggle notifies.
async fn reconcile_wait(notify: &tokio::sync::Notify) {
    tokio::select! {
        () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
        () = notify.notified() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> PlatformSettings {
        PlatformSettings {
            domain: "litebin.dev".into(),
            dashboard_subdomain: "dash".into(),
            poke_subdomain: "poke".into(),
            dns_target: String::new(),
            previous_domain: "old.dev".into(),
            staged_domain: String::new(),
        }
    }

    #[tokio::test]
    async fn master_proxy_zone_wildcards_domain_and_grace() {
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, is_background INTEGER DEFAULT 0, custom_domain TEXT, node_id TEXT)")
            .execute(&db)
            .await
            .unwrap();
        let zone = desired_internal_zone(&db, &snap(), "203.0.113.5", &RoutingMode::MasterProxy, "172.17.0.1").await;
        assert_eq!(zone.answer_ip, "172.17.0.1");
        assert_eq!(zone.wildcard_domains, vec!["litebin.dev".to_string(), "old.dev".to_string()]);
        // No custom domains in the DB → none enumerated (and no public lookups).
        assert!(zone.exact_names.is_empty());
    }

    #[test]
    fn sidecar_spec_publishes_53_on_bind_ip_with_caps() {
        let spec = dns_sidecar_spec(
            "litebin-orchestrator",
            Some("sha256:abc".into()),
            "litebin-network",
            "172.17.0.1",
            "tok",
            &["1.1.1.1".into()],
        );
        assert_eq!(spec.name, "litebin-dns");
        assert_eq!(spec.cmd, vec!["/app/litebin-orchestrator".to_string(), "dns".to_string()]);
        assert_eq!(spec.ports.len(), 2);
        assert!(spec.ports.iter().all(|p| p.host_ip == "172.17.0.1" && p.host_port == 53 && p.container_port == 53));
        assert_eq!(spec.memory_limit, Some(64 * 1024 * 1024));
        assert_eq!(spec.memory_reservation, Some(12 * 1024 * 1024));
        assert!(spec.env.iter().any(|e| e == "DNS_PUSH_TOKEN=tok"));
        assert!(spec.env.iter().any(|e| e == "UPSTREAM_DNS=1.1.1.1"));
    }
}
