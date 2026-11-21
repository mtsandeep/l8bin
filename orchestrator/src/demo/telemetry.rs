//! Ephemeral demo visitor telemetry — in-memory only, never persisted:
//! sessions keep ip/UA (parsed), referrer, landing path and per-page hits;
//! shared mode keeps per-IP hit counts. Capped, TTL'd, wiped on restart.

use std::sync::LazyLock;

use dashmap::DashMap;

const MAX_SESSIONS: usize = 200; // most recent kept, oldest evicted
const MAX_SHARED_IPS: usize = 100; // per parent, least recently seen evicted
const MAX_PAGES: usize = 30; // per session, least-hit evicted
const MAX_STR: usize = 256; // ua/referer truncation
const MAX_PATH: usize = 200;
const ENTRY_TTL_SECS: i64 = 24 * 60 * 60;

#[derive(Clone)]
pub struct PageHit {
    pub path: String,
    pub hits: i64,
}

#[derive(Clone)]
pub struct SessionDetail {
    pub ip: String,
    pub user_agent: String,
    pub browser: String,
    pub os: String,
    pub device: String, // desktop | mobile | tablet | bot
    pub bot: bool,
    pub referer: String,
    pub landing: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub hits: i64,
    pub pages: Vec<PageHit>,
}

#[derive(Clone)]
pub struct IpHits {
    pub hits: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

static SESSIONS: LazyLock<DashMap<String, SessionDetail>> = LazyLock::new(DashMap::new);
static SHARED: LazyLock<DashMap<(String, String), IpHits>> = LazyLock::new(DashMap::new); // (parent, ip)

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn expired(t: i64) -> bool {
    now() - t > ENTRY_TTL_SECS
}

fn cut(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Best-effort UA digest for the dashboard — order matters (Edge/Opera
/// carry "Chrome" in their UA strings; bots override everything).
fn parse_ua(ua: &str) -> (String, String, String, bool) {
    let l = ua.to_ascii_lowercase();
    let bot = [
        "bot",
        "crawl",
        "spider",
        "slurp",
        "headless",
        "curl/",
        "wget",
        "python-requests",
        "node-fetch",
        "go-http-client",
        "postman",
    ]
    .iter()
    .any(|s| l.contains(s));
    let browser = if l.contains("edg/") {
        "Edge"
    } else if l.contains("opr/") || l.contains("opera") {
        "Opera"
    } else if l.contains("firefox") {
        "Firefox"
    } else if l.contains("chrome") {
        "Chrome"
    } else if l.contains("safari") {
        "Safari"
    } else {
        "Other"
    };
    let os = if l.contains("android") {
        "Android"
    } else if l.contains("iphone") || l.contains("ipad") {
        "iOS"
    } else if l.contains("windows") {
        "Windows"
    } else if l.contains("mac os") || l.contains("macintosh") {
        "macOS"
    } else if l.contains("linux") {
        "Linux"
    } else {
        "Other"
    };
    let device = if bot {
        "bot"
    } else if l.contains("ipad") || l.contains("tablet") {
        "tablet"
    } else if l.contains("mobi") || l.contains("iphone") || l.contains("android") {
        "mobile"
    } else {
        "desktop"
    };
    (browser.into(), os.into(), device.into(), bot)
}

/// Best-effort "page" filter — SPA routes and classic pages have no file
/// extension; assets and the gatekeeper's own endpoints don't count.
fn is_page(path: &str) -> bool {
    if path.starts_with("/__demo__") {
        return false;
    }
    !path.rsplit('/').next().unwrap_or("").contains('.')
}

pub fn record_claim(session_id: &str, ip: &str, user_agent: &str, referer: &str, landing: &str) {
    SESSIONS.retain(|_, d| !expired(d.last_seen));
    while SESSIONS.len() >= MAX_SESSIONS {
        let Some(oldest) = SESSIONS.iter().min_by_key(|e| e.value().first_seen).map(|e| e.key().clone()) else {
            break;
        };
        SESSIONS.remove(&oldest);
    }
    let (browser, os, device, bot) = parse_ua(user_agent);
    let t = now();
    let landing = cut(landing, MAX_PATH);
    SESSIONS.insert(
        session_id.to_string(),
        SessionDetail {
            ip: ip.to_string(),
            user_agent: cut(user_agent, MAX_STR),
            browser,
            os,
            device,
            bot,
            referer: cut(referer, MAX_STR),
            landing: landing.clone(),
            first_seen: t,
            last_seen: t,
            hits: 1,
            pages: vec![PageHit { path: landing, hits: 1 }],
        },
    );
}

pub fn record_hit(session_id: &str, path: &str) {
    let Some(mut d) = SESSIONS.get_mut(session_id) else { return };
    d.hits += 1;
    d.last_seen = now();
    if !is_page(path) {
        return;
    }
    let path = cut(path, MAX_PATH);
    if let Some(p) = d.pages.iter_mut().find(|p| p.path == path) {
        p.hits += 1;
    } else {
        if d.pages.len() >= MAX_PAGES
            && let Some(i) = d.pages.iter().enumerate().min_by_key(|(_, p)| p.hits).map(|(i, _)| i)
        {
            d.pages.swap_remove(i);
        }
        d.pages.push(PageHit { path, hits: 1 });
    }
}

pub fn session_detail(session_id: &str) -> Option<SessionDetail> {
    SESSIONS.get(session_id).map(|d| d.value().clone())
}

pub fn record_shared(parent_id: &str, ip: &str) {
    let key = (parent_id.to_string(), ip.to_string());
    let t = now();
    if let Some(mut h) = SHARED.get_mut(&key) {
        h.hits += 1;
        h.last_seen = t;
    } else {
        evict_shared(parent_id);
        SHARED.insert(key, IpHits { hits: 1, first_seen: t, last_seen: t });
    }
}

fn evict_shared(parent_id: &str) {
    SHARED.retain(|_, h| !expired(h.last_seen));
    let count = SHARED.iter().filter(|e| e.key().0 == parent_id).count();
    if count < MAX_SHARED_IPS {
        return;
    }
    let mut ips: Vec<(String, i64)> =
        SHARED.iter().filter(|e| e.key().0 == parent_id).map(|e| (e.key().1.clone(), e.value().last_seen)).collect();
    ips.sort_by_key(|(_, last)| *last);
    for (ip, _) in ips.into_iter().take(count - MAX_SHARED_IPS + 1) {
        SHARED.remove(&(parent_id.to_string(), ip));
    }
}

/// Per-IP traffic for a shared demo, heaviest first.
pub fn shared_traffic(parent_id: &str) -> Vec<(String, IpHits)> {
    let mut out: Vec<(String, IpHits)> =
        SHARED.iter().filter(|e| e.key().0 == parent_id).map(|e| (e.key().1.clone(), e.value().clone())).collect();
    out.sort_by(|a, b| b.1.hits.cmp(&a.1.hits));
    out
}
