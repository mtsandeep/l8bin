//! HTTP client construction for the CLI.
//!
//! Every client resolves `*.localhost` locally, per RFC 6761 — `.localhost`
//! is reserved to always mean this machine. OS resolvers vary (macOS and
//! systemd-resolved answer it; Windows and plain-glibc Linux don't), so the
//! CLI implements the rule itself: `l8b login --server http://l8bin.localhost`
//! behaves the same on every OS, as it already does in the browser.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// True for subdomains of the RFC 6761 `.localhost` domain. Bare `localhost`
/// is left to the OS, which handles it on every platform.
pub fn is_localhost_domain(host: &str) -> bool {
    let h = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    h != "localhost" && h.ends_with(".localhost")
}

/// `.localhost` subdomains → loopback; everything else → the OS resolver
/// (blocking getaddrinfo off the async loop, as reqwest's own resolver does).
struct LocalhostAware;

impl Resolve for LocalhostAware {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        if is_localhost_domain(&host) {
            // Port 0: reqwest substitutes the URL's port.
            let addrs: Addrs = Box::new(
                vec![SocketAddr::from(([127, 0, 0, 1], 0)), SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 0))].into_iter(),
            );
            return Box::pin(async move { Ok(addrs) });
        }
        Box::pin(async move {
            let addrs = tokio::task::spawn_blocking(move || (host.as_str(), 0).to_socket_addrs())
                .await
                .map_err(|e| Box::new(e) as BoxError)?
                .map_err(|e| Box::new(e) as BoxError)?;
            Ok(Box::new(addrs) as Addrs)
        })
    }
}

/// `reqwest::Client::builder()` with the CLI's resolver applied.
pub fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(Arc::new(LocalhostAware))
}

/// `reqwest::Client::new()` with the CLI's resolver applied.
pub fn client() -> reqwest::Client {
    builder().build().expect("client with default options always builds")
}

#[cfg(test)]
mod tests {
    use super::is_localhost_domain as f;

    #[test]
    fn subdomains_of_localhost_match() {
        assert!(f("l8bin.localhost"));
        assert!(f("poke.l8bin.localhost"));
        assert!(f("l8bin.localhost.")); // FQDN trailing dot
        assert!(f("L8BIN.LOCALHOST"));
    }

    #[test]
    fn everything_else_falls_through_to_the_os() {
        assert!(!f("localhost")); // OS special-case covers bare localhost
        assert!(!f("example.com"));
        assert!(!f("l8bin.localhost.evil.com"));
        assert!(!f("notlocalhost"));
        assert!(!f(""));
    }
}
