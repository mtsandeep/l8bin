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
    if let Some(ua) = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()) {
        let ua = ua.to_lowercase();
        if ua.contains("bot") || ua.contains("crawler") || ua.contains("spider") {
            return true;
        }
    }
    false
}

/// Wake-time client class: same silent hold for all, different fallback — Document gets the waiting page, Asset
/// holds on (no unstyled half-loads), Api gets the 503 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeClient {
    /// Browser navigation (`Accept: text/html`, `Sec-Fetch-Dest: document/iframe`).
    Document,
    /// Browser subresource (`Sec-Fetch-Dest: style/script/image/font/...`).
    Asset,
    /// Everything else: curl, SDKs, fetch()/EventSource (`Sec-Fetch-Dest: empty`),
    /// and known bots — held like everyone, then the 503 contract.
    Api,
}

/// `Sec-Fetch-Dest` values that identify a browser subresource fetch.
const ASSET_DESTS: &[&str] = &["style", "script", "image", "font", "audio", "video", "track", "manifest"];

/// Classify a request; bots always map to Api (held, but never the waiting page).
pub fn classify_wake_client(headers: &HeaderMap) -> WakeClient {
    if let Some(ua) = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()) {
        let ua = ua.to_lowercase();
        if ua.contains("bot") || ua.contains("crawler") || ua.contains("spider") {
            return WakeClient::Api;
        }
    }
    if let Some(dest) = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok()) {
        let dest = dest.trim().to_lowercase();
        if dest == "document" || dest == "iframe" {
            return WakeClient::Document;
        }
        if ASSET_DESTS.contains(&dest.as_str()) {
            return WakeClient::Asset;
        }
        // "empty" (fetch/XHR/EventSource) and unknown values → Api
        return WakeClient::Api;
    }
    // No Sec-Fetch-Dest (curl, SDKs, ancient browsers) — fall back to Accept.
    let accepts_html = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("text/html"))
        .unwrap_or(false);
    if accepts_html { WakeClient::Document } else { WakeClient::Api }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(http::HeaderName::try_from(*k).unwrap(), http::HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn classify_browser_navigation() {
        let h = headers(&[
            ("accept", "text/html,application/xhtml+xml"),
            ("sec-fetch-dest", "document"),
            ("user-agent", "Mozilla/5.0 Chrome/120"),
        ]);
        assert_eq!(classify_wake_client(&h), WakeClient::Document);
    }

    #[test]
    fn classify_navigation_without_sec_fetch() {
        let h = headers(&[("accept", "text/html"), ("user-agent", "OldBrowser/1.0")]);
        assert_eq!(classify_wake_client(&h), WakeClient::Document);
    }

    #[test]
    fn classify_subresources() {
        for dest in ["style", "script", "image", "font"] {
            let h = headers(&[("accept", "text/css,*/*;q=0.1"), ("sec-fetch-dest", dest)]);
            assert_eq!(classify_wake_client(&h), WakeClient::Asset, "dest={dest}");
        }
    }

    #[test]
    fn classify_api_clients() {
        // curl: no Sec-Fetch-Dest, no text/html accept
        let h = headers(&[("accept", "*/*"), ("user-agent", "curl/8.0")]);
        assert_eq!(classify_wake_client(&h), WakeClient::Api);
        // fetch()/XHR/EventSource: Sec-Fetch-Dest: empty
        let h = headers(&[("accept", "application/json"), ("sec-fetch-dest", "empty")]);
        assert_eq!(classify_wake_client(&h), WakeClient::Api);
    }

    #[test]
    fn classify_bots_are_api_even_with_html_accept() {
        let h = headers(&[("accept", "text/html"), ("sec-fetch-dest", "document"), ("user-agent", "Googlebot/2.1")]);
        assert_eq!(classify_wake_client(&h), WakeClient::Api);
    }

    #[test]
    fn classify_iframe_is_document() {
        let h = headers(&[("accept", "text/html"), ("sec-fetch-dest", "iframe")]);
        assert_eq!(classify_wake_client(&h), WakeClient::Document);
    }
}
