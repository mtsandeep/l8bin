//! Per-IP fixed-window rate limiting for unauthenticated auth endpoints.
//!
//! Keyed by the *last* `X-Forwarded-For` entry (appended by the proxy), so
//! clients cannot rotate the limit away by sending fake XFF headers. Requests
//! without XFF (direct/dev access, the test harness) are exempt — public
//! traffic always arrives through Caddy, which sets the header.

use std::sync::OnceLock;

use axum::http::HeaderMap;

const WINDOW_SECS: u64 = 60;

#[derive(Clone, Copy)]
pub enum Policy {
    Login,
    DeviceStart,
    DeviceToken,
}

impl Policy {
    fn limit(self) -> u32 {
        match self {
            Policy::Login => 10,
            Policy::DeviceStart => 5,
            Policy::DeviceToken => 30,
        }
    }
}

fn buckets() -> &'static dashmap::DashMap<String, (u32, std::time::Instant)> {
    static BUCKETS: OnceLock<dashmap::DashMap<String, (u32, std::time::Instant)>> = OnceLock::new();
    BUCKETS.get_or_init(dashmap::DashMap::new)
}

/// Extract the limiting key: the proxy-appended (last) XFF entry.
fn client_key(headers: &HeaderMap) -> Option<String> {
    let xff = headers.get("x-forwarded-for")?.to_str().ok()?;
    xff.split(',').next_back().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Returns true when the request is allowed. Records the attempt when allowed.
pub fn allow(headers: &HeaderMap, policy: Policy) -> bool {
    let Some(key) = client_key(headers) else { return true };
    let now = std::time::Instant::now();

    let mut entry = buckets().entry(format!("{key}:{}", policy.name())).or_insert((0, now));
    let (count, window_start) = entry.value_mut();
    if now.duration_since(*window_start).as_secs() >= WINDOW_SECS {
        *count = 0;
        *window_start = now;
    }
    *count += 1;
    *count <= policy.limit()
}

impl Policy {
    fn name(self) -> &'static str {
        match self {
            Policy::Login => "login",
            Policy::DeviceStart => "device-start",
            Policy::DeviceToken => "device-token",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(ip: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", ip.parse().unwrap());
        h
    }

    #[test]
    fn allows_up_to_limit_then_blocks() {
        let h = headers_with("203.0.113.10");
        for _ in 0..Policy::Login.limit() {
            assert!(allow(&h, Policy::Login));
        }
        assert!(!allow(&h, Policy::Login));
    }

    #[test]
    fn last_xff_entry_is_the_key_spoofing_first_entry_does_not_help() {
        let spoofed = headers_with("1.2.3.4, 203.0.113.11");
        for _ in 0..Policy::Login.limit() {
            assert!(allow(&spoofed, Policy::Login));
        }
        // Same real client IP via a different fake prefix — still blocked.
        let spoofed_again = headers_with("9.9.9.9, 203.0.113.11");
        assert!(!allow(&spoofed_again, Policy::Login));
    }

    #[test]
    fn missing_xff_is_exempt() {
        let h = HeaderMap::new();
        for _ in 0..1000 {
            assert!(allow(&h, Policy::Login));
        }
    }

    #[test]
    fn policies_are_independent() {
        let h = headers_with("203.0.113.12");
        assert!(allow(&h, Policy::Login));
        assert!(allow(&h, Policy::DeviceToken));
    }
}
