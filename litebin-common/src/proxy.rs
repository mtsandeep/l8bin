use http::{HeaderMap, header};

/// Hop-by-hop headers that must not be forwarded when proxying.
/// Per RFC 2616 §13.5.1 — these headers are meaningful only for a
/// single transport-level connection and must not be sent to the upstream.
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "transfer-encoding",
    "upgrade",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "trailer",
];

/// Returns true if the given header name is a hop-by-hop header.
#[inline]
pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name.to_lowercase().as_str())
}

/// Check if the client should get a JSON error response instead of the HTML
/// loading page.  Returns true for API clients (no `Accept: text/html`) and
/// known bots (Googlebot, etc.) so they don't index the loading spinner or
/// retry endlessly.
pub fn wants_json(headers: &HeaderMap) -> bool {
    if !headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("text/html"))
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(ua) = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    {
        let ua = ua.to_lowercase();
        if ua.contains("bot") || ua.contains("crawler") || ua.contains("spider") {
            return true;
        }
    }
    false
}
