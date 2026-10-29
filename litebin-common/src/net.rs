use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Dual-stack wildcard (`[::]`) when the kernel has IPv6, else `0.0.0.0` —
/// IPv4 must stay reachable even on hosts with IPv6 disabled.
pub fn wildcard_addr(port: u16) -> SocketAddr {
    if std::net::TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))).is_ok() {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))
    } else {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
    }
}

/// Detect the host's public IP address by querying external services.
/// Tries multiple providers in order — returns the first successful result.
/// Each provider gets 3s total before we move to the next.
/// Returns `None` only if all providers fail.
pub async fn detect_public_ip() -> Option<String> {
    let providers: &[&str] = &[
        "https://api.ipify.org",
        "https://checkip.amazonaws.com",
        "https://ipv4.icanhazip.com",
        "https://v4.ident.me",
    ];

    for url in providers {
        let client = reqwest::Client::builder()
            .connect_timeout(crate::timeouts::CONNECT_LOCAL)
            .timeout(crate::timeouts::PUBLIC_IP_PROBE_TOTAL)
            .build()
            .ok()?;

        if let Some(ip) = try_fetch_ip(&client, url).await {
            return Some(ip);
        }
    }

    None
}

async fn try_fetch_ip(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let ip = resp.text().await.ok()?;
    let ip = ip.trim().to_string();
    if ip.parse::<std::net::Ipv4Addr>().is_ok() { Some(ip) } else { None }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// DNS resolver that returns every family with IPv4 first.
///
/// On IPv6-enabled Docker networks names resolve AAAA-first but most images bind
/// IPv4 only, and stock reqwest has no family fallback — the waker/dashboard
/// proxies 502 against healthy containers. IPv4 first fixes dual-stack networks;
/// IPv6-only hosts still work (IPv6 is all they resolve to).
struct Ipv4FirstResolver;

impl Resolve for Ipv4FirstResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::task::spawn_blocking(move || (host.as_str(), 0).to_socket_addrs())
                .await
                .map_err(|e| Box::new(e) as BoxError)?
                .map_err(|e| Box::new(e) as BoxError)?
                .collect::<Vec<SocketAddr>>();
            let (v4, v6): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|a| matches!(a.ip(), IpAddr::V4(_)));
            Ok(Box::new(v4.into_iter().chain(v6)) as Addrs)
        })
    }
}

/// `reqwest::Client::builder()` with litebin's DNS resolver applied —
/// required for any client dialing Docker-network names (IPv6-enabled networks).
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(Arc::new(Ipv4FirstResolver))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// One-shot HTTP server: accepts any request, replies "ok", closes.
    async fn ok_listener(bind: IpAddr) -> SocketAddr {
        let ln = tokio::net::TcpListener::bind(SocketAddr::new(bind, 0)).await.unwrap();
        let addr = ln.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = ln.accept().await else { break };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
                });
            }
        });
        addr
    }

    /// Resolver returning fixed addresses — port 0, reqwest substitutes the URL's port.
    struct Fixed(Vec<SocketAddr>);
    impl Resolve for Fixed {
        fn resolve(&self, _name: Name) -> Resolving {
            let addrs: Addrs = Box::new(self.0.clone().into_iter());
            Box::pin(async move { Ok(addrs) })
        }
    }

    fn client_with(addrs: Vec<SocketAddr>) -> reqwest::Client {
        reqwest::Client::builder().dns_resolver(Arc::new(Fixed(addrs))).build().unwrap()
    }

    #[tokio::test]
    async fn falls_back_from_refused_ipv6_to_ipv4() {
        let live = ok_listener(IpAddr::V4(Ipv4Addr::LOCALHOST)).await; // nothing listens on ::1:same_port
        let client = client_with(vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        ]);
        let body =
            client.get(format!("http://fallback.test:{}", live.port())).send().await.unwrap().text().await.unwrap();
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn falls_back_from_refused_ipv4_to_ipv6() {
        let live = ok_listener(IpAddr::V6(Ipv6Addr::LOCALHOST)).await; // nothing listens on 127.0.0.1:same_port
        let client = client_with(vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
        ]);
        let body =
            client.get(format!("http://fallback.test:{}", live.port())).send().await.unwrap().text().await.unwrap();
        assert_eq!(body, "ok");
    }

    #[test]
    fn wildcard_addr_keeps_port_and_covers_all_interfaces() {
        let a = wildcard_addr(5080);
        assert_eq!(a.port(), 5080);
        assert!(a.ip().is_unspecified());
    }

    #[tokio::test]
    async fn resolver_orders_ipv4_before_ipv6() {
        let resolving = Ipv4FirstResolver.resolve("localhost".parse::<Name>().unwrap());
        let addrs: Vec<SocketAddr> = resolving.await.unwrap().collect();
        assert!(!addrs.is_empty());
        let first_v6 = addrs.iter().position(|a| a.is_ipv6());
        let last_v4 = addrs.iter().rposition(|a| a.is_ipv4());
        if let (Some(first_v6), Some(last_v4)) = (first_v6, last_v4) {
            assert!(last_v4 < first_v6, "IPv4 answers must precede IPv6: {addrs:?}");
        }
    }
}
