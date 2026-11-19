//! `litebin-dns` — split-horizon resolver sidecar (`dns` subcommand in the
//! orchestrator and agent binaries).
//!
//! Serves UDP+TCP :53 (published on the host's default-bridge gateway). Zone
//! names answer with the host gateway (A) or an empty NOERROR (AAAA, anything
//! else) so same-host traffic reaches Caddy instead of hairpinning to the
//! host's public IP; everything else is relayed upstream verbatim. A
//! token-gated HTTP listener accepts full-zone snapshots from the orchestrator.
//! See docs/topics/domains.md.

mod http;
mod zone;

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hickory_proto::op::Message;
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub use zone::ZoneSpec;

/// Shared zone state: swapped atomically by `/load`, read per query.
pub type SharedZone = Arc<RwLock<ZoneSpec>>;

const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TCP_MESSAGE: usize = 65_535;

pub struct DnsServerConfig {
    pub dns_listen: SocketAddr,
    pub http_listen: SocketAddr,
    pub push_token: Option<String>,
    pub upstreams: Vec<SocketAddr>,
    pub zone_path: std::path::PathBuf,
}

impl DnsServerConfig {
    pub fn from_env() -> Self {
        let dns_listen = std::env::var("DNS_LISTEN").unwrap_or_else(|_| "0.0.0.0:53".into());
        let http_listen = std::env::var("DNS_HTTP_LISTEN").unwrap_or_else(|_| "0.0.0.0:5380".into());
        let push_token = std::env::var("DNS_PUSH_TOKEN").ok().filter(|t| !t.is_empty());
        let zone_path = std::env::var("DNS_ZONE_PATH").unwrap_or_else(|_| "data/dns-zone.json".into());
        Self {
            dns_listen: dns_listen.parse().expect("DNS_LISTEN must be a socket addr"),
            http_listen: http_listen.parse().expect("DNS_HTTP_LISTEN must be a socket addr"),
            push_token,
            upstreams: upstreams_from_env(),
            zone_path: zone_path.into(),
        }
    }
}

/// UPSTREAM_DNS (comma-separated, bare IPs get :53); default = the host's
/// first non-loopback nameserver, else 1.1.1.1.
fn upstreams_from_env() -> Vec<SocketAddr> {
    let parse = |s: &str| -> Option<SocketAddr> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:53") };
        with_port.parse().ok()
    };
    if let Ok(raw) = std::env::var("UPSTREAM_DNS")
        && !raw.trim().is_empty()
    {
        let upstreams: Vec<_> = raw.split(',').filter_map(parse).collect();
        if !upstreams.is_empty() {
            return upstreams;
        }
    }
    let mut upstreams = Vec::new();
    if let Ok(conf) = std::fs::read_to_string("/etc/resolv.conf")
        && let Some(found) = conf.lines().find_map(|line| {
            let rest = line.strip_prefix("nameserver")?.trim();
            (!rest.starts_with("127.")).then(|| parse(rest)).flatten()
        })
    {
        upstreams.push(found);
    }
    if upstreams.is_empty()
        && let Some(fallback) = parse("1.1.1.1")
    {
        upstreams.push(fallback);
    }
    upstreams
}

/// Answer a query from the local zone, or `None` when it must be forwarded.
/// Pure: unit-testable without sockets.
pub(crate) fn answer_local(query: &Message, zone: &ZoneSpec) -> Option<Message> {
    let question = query.queries.first()?;
    let qname = question.name().to_utf8();
    if !zone.matches(&qname) {
        return None;
    }
    let mut response = query.clone().into_response();
    response.metadata.recursion_available = true;
    // Zone names have only the gateway A record. AAAA answers empty
    // NOERROR so dual-stack clients fall back to A instead of dialing
    // the public v6 record (which hairpins).
    if question.query_type() == RecordType::A
        && let Ok(ip) = zone.answer_ip.parse()
    {
        response.add_answer(Record::from_rdata(question.name().clone(), zone.ttl_secs, RData::A(A(ip))));
    }
    Some(response)
}

/// Entry point of the `dns` subcommand. Runs until the process is killed.
pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = DnsServerConfig::from_env();
    let zone = ZoneSpec::load(&config.zone_path).unwrap_or_default();
    tracing::info!(
        zone_path = %config.zone_path.display(),
        wildcards = zone.wildcard_domains.len(),
        exact = zone.exact_names.len(),
        upstreams = ?config.upstreams,
        "litebin-dns starting"
    );
    let shared: SharedZone = Arc::new(RwLock::new(zone));

    let dns = serve_dns(config.dns_listen, Arc::clone(&shared), config.upstreams.clone());
    let http = http::serve(config.http_listen, Arc::clone(&shared), config.push_token.clone());
    let persistence = zone_persistence(Arc::clone(&shared), config.zone_path.clone());

    tokio::try_join!(dns, http, persistence)?;
    Ok(())
}

/// Re-persist the zone whenever `/load` swaps it (crash-safe best effort —
/// the orchestrator re-pushes on boot regardless).
async fn zone_persistence(shared: SharedZone, path: std::path::PathBuf) -> anyhow::Result<()> {
    let mut last = shared.read().expect("zone lock").hash_hex();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let current = shared.read().expect("zone lock").hash_hex();
        if current != last {
            let snapshot = shared.read().expect("zone lock").clone();
            match snapshot.save(&path) {
                Ok(()) => {
                    tracing::info!(zone_sha256 = %current, "dns zone persisted");
                    last = current;
                }
                Err(e) => tracing::warn!(error = %e, "failed to persist dns zone"),
            }
        }
    }
}

async fn serve_dns(listen: SocketAddr, shared: SharedZone, upstreams: Vec<SocketAddr>) -> anyhow::Result<()> {
    let udp = Arc::new(tokio::net::UdpSocket::bind(listen).await?);
    let tcp = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "dns udp+tcp listening");
    let handle = Arc::new(QueryHandler { shared, upstreams });

    let udp_task = tokio::spawn({
        let handle = Arc::clone(&handle);
        let udp = Arc::clone(&udp);
        async move {
            let mut buf = vec![0u8; MAX_TCP_MESSAGE];
            loop {
                let Ok((len, peer)) = udp.recv_from(&mut buf).await else { continue };
                let query = buf[..len].to_vec();
                let handle = Arc::clone(&handle);
                let udp = Arc::clone(&udp);
                tokio::spawn(async move {
                    if let Some(response) = handle.respond(&query).await {
                        let _ = udp.send_to(&response, peer).await;
                    }
                });
            }
        }
    });

    let tcp_task = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move {
            loop {
                let Ok((mut stream, _peer)) = tcp.accept().await else { continue };
                let handle = Arc::clone(&handle);
                tokio::spawn(async move {
                    // Clients may pipeline; serve until EOF or error.
                    loop {
                        let mut len_buf = [0u8; 2];
                        if stream.read_exact(&mut len_buf).await.is_err() {
                            break;
                        }
                        let mut msg = vec![0u8; u16::from_be_bytes(len_buf) as usize];
                        if stream.read_exact(&mut msg).await.is_err() {
                            break;
                        }
                        let Some(response) = handle.respond(&msg).await else { continue };
                        if stream.write_all(&framed_message(&response)).await.is_err() {
                            break;
                        }
                    }
                });
            }
        }
    });

    let (udp_res, tcp_res) = tokio::join!(udp_task, tcp_task);
    udp_res?;
    tcp_res?;
    Ok(())
}

struct QueryHandler {
    shared: SharedZone,
    upstreams: Vec<SocketAddr>,
}

impl QueryHandler {
    /// Parse, answer from the zone or relay upstream. `None` = drop
    /// (unparseable query, or every upstream failed).
    async fn respond(&self, query_bytes: &[u8]) -> Option<Vec<u8>> {
        let query = Message::from_vec(query_bytes).ok()?;
        if let Some(local) = answer_local(&query, &self.shared.read().expect("zone lock")) {
            return local.to_vec().ok();
        }
        forward(query_bytes, &self.upstreams).await
    }
}

fn framed_message(bytes: &[u8]) -> Vec<u8> {
    let mut framed = (bytes.len() as u16).to_be_bytes().to_vec();
    framed.extend_from_slice(bytes);
    framed
}

/// Verbatim byte relay to the first upstream that answers, trying in order.
async fn forward(query_bytes: &[u8], upstreams: &[SocketAddr]) -> Option<Vec<u8>> {
    for upstream in upstreams {
        if let Some(response) = forward_one(query_bytes, *upstream).await {
            return Some(response);
        }
    }
    tracing::warn!(?upstreams, "all upstreams failed");
    None
}

async fn forward_one(query_bytes: &[u8], upstream: SocketAddr) -> Option<Vec<u8>> {
    tokio::time::timeout(FORWARD_TIMEOUT, async {
        if upstream.is_ipv4() {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
            socket.send_to(query_bytes, upstream).await.ok()?;
            let mut buf = vec![0u8; MAX_TCP_MESSAGE];
            let (len, _) = socket.recv_from(&mut buf).await.ok()?;
            Some(buf[..len].to_vec())
        } else {
            let mut stream = tokio::net::TcpStream::connect(upstream).await.ok()?;
            stream.write_all(&framed_message(query_bytes)).await.ok()?;
            let mut len_buf = [0u8; 2];
            stream.read_exact(&mut len_buf).await.ok()?;
            let mut response = vec![0u8; u16::from_be_bytes(len_buf) as usize];
            stream.read_exact(&mut response).await.ok()?;
            Some(response)
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::{MessageType, Query};
    use hickory_proto::rr::Name;

    fn query(name: &str, rt: RecordType) -> Message {
        let mut q = Message::query();
        q.metadata.id = 7;
        q.metadata.recursion_desired = true;
        q.add_query(Query::query(Name::from_utf8(name).unwrap(), rt));
        q
    }

    fn zone() -> ZoneSpec {
        ZoneSpec {
            answer_ip: "172.17.0.1".into(),
            ttl_secs: 5,
            wildcard_domains: vec!["litebin.dev".into()],
            exact_names: vec!["app.example.com".into()],
        }
    }

    #[test]
    fn zone_a_query_answers_gateway_ip() {
        let response = answer_local(&query("dash.litebin.dev", RecordType::A), &zone()).unwrap();
        assert_eq!(response.metadata.message_type, MessageType::Response);
        let answer = response.answers.first().unwrap();
        assert_eq!(answer.record_type(), RecordType::A);
        assert_eq!(answer.ttl, 5);
        match &answer.data {
            RData::A(a) => assert_eq!(a.0.to_string(), "172.17.0.1"),
            other => panic!("unexpected rdata: {other:?}"),
        }
    }

    #[test]
    fn zone_aaaa_answers_empty_noerror() {
        let response = answer_local(&query("dash.litebin.dev", RecordType::AAAA), &zone()).unwrap();
        assert_eq!(response.metadata.message_type, MessageType::Response);
        assert!(response.answers.is_empty());
    }

    #[test]
    fn non_zone_query_is_none_for_forwarding() {
        assert!(answer_local(&query("github.com", RecordType::A), &zone()).is_none());
    }

    #[test]
    fn response_echoes_question_and_id() {
        let response = answer_local(&query("app.example.com", RecordType::A), &zone()).unwrap();
        assert_eq!(response.metadata.id, 7);
        assert_eq!(response.queries.len(), 1);
        assert_eq!(response.queries[0].name().to_utf8(), "app.example.com");
    }
}
