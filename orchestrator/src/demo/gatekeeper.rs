//! Demo gatekeeper — the visitor-facing surface of demo mode, hooked into the
//! waker's host resolution (demo hosts resolve to the parent project, so this
//! runs before the normal proxy/wake flow). Shared mode proxies to the shared
//! instance; session mode maps a cookie (`l8b_demo`) to a session row and its
//! instance. Both proxy through the waker's local/remote paths, and direct
//! hits on session-instance hosts bounce to the demo host.

use std::sync::{Arc, LazyLock};

use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use sha2::{Digest, Sha256};

use crate::demo::{self, DemoConfig};
use crate::AppState;
use litebin_common::types::{Project, ProjectStatus};

pub const DEMO_COOKIE: &str = "l8b_demo";

const PENDING_TITLE: &str = "Starting your demo…";
const PENDING_MSG: &str = "Your demo instance is starting.";
const DEMO_PREFIX: &str = "/__demo__/";

/// Per-parent claim serialization — capacity check + session INSERT must be
/// atomic against concurrent first-hitters (all claims are in-process).
static CLAIM_LOCKS: LazyLock<DashMap<String, Arc<tokio::sync::Mutex<()>>>> =
    LazyLock::new(|| DashMap::new());

async fn claim_lock(parent_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    CLAIM_LOCKS.entry(parent_id.to_string()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
}

/// Entry point from the waker. `None` = not demo traffic, caller continues
/// its normal flow. Takes the body by reference — non-demo requests (the
/// common case) never pay for a clone.
pub async fn handle_wake(
    state: &AppState,
    project: &Project,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &axum::body::Bytes,
) -> Option<Response> {
    // Demo instance host hit directly (only resolvable where a wildcard or
    // shared record covers it): a valid session for THIS instance falls
    // through to the normal proxy; the operator's edit/shared instances serve
    // normally too (operator surface — no bar); anything else bounces to the
    // demo host.
    if let Some(parent_id) = project.demo_parent_id.as_deref() {
        let parent_config = demo::get_config(&state.db, parent_id).await.ok().flatten();
        let long_lived = parent_config
            .as_ref()
            .map(|c| c.edit_instance_id.as_deref() == Some(project.id.as_str()) || c.shared_instance_id.as_deref() == Some(project.id.as_str()))
            .unwrap_or(false);
        if long_lived {
            return None;
        }
        let valid = session_for_request(state, headers, parent_id)
            .await
            .map(|s| s.instance == project.id)
            .unwrap_or(false);
        if valid {
            return None;
        }
        return Some(redirect_to_demo_host(state, parent_id).await);
    }

    let config = demo::get_config(&state.db, &project.id).await.ok().flatten()?;

    // Shared mode: visitors hit the shared instance; the bar renders on it.
    // The parent itself is a pure template and never serves.
    if config.mode != "session" {
        let Some(shared_id) = config.shared_instance_id.clone() else {
            return None; // instances not provisioned yet — normal flow until the janitor creates them
        };
        if uri.path().starts_with(DEMO_PREFIX) {
            return Some(status_endpoint(state, headers, &config, &project.id).await);
        }
        let shared: Option<Project> = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
            .bind(&shared_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
        let Some(shared) = shared else { return None };
        if !matches!(shared.status, ProjectStatus::Running | ProjectStatus::Degraded) {
            if !demo::mid_operation(&shared.status) && shared.auto_start_enabled {
                spawn_start(state, &shared.id);
            }
            return Some(page(PENDING_TITLE, PENDING_MSG, None, true));
        }
        return Some(
            inject_bar(
                proxy_session(state, &shared_id, method, uri, headers, body.clone()).await,
                &config,
            )
            .await,
        );
    }

    // Reserved gatekeeper endpoints.
    if uri.path().starts_with(DEMO_PREFIX) {
        return Some(status_endpoint(state, headers, &config, &project.id).await);
    }

    // Existing session → proxy.
    if let Some(session) = session_for_request(state, headers, &project.id).await {
        if session.expires_at <= chrono::Utc::now().timestamp() {
            let _ = sqlx::query("UPDATE demo_sessions SET status = 'expired' WHERE id = ? AND status = 'active'")
                .bind(&session.id)
                .execute(&state.db)
                .await;
            return Some(page("Demo session ended", "Reload to start a new one — you'll get a fresh instance.", None, false));
        }
        let _ = sqlx::query("UPDATE demo_sessions SET last_seen_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().timestamp())
            .bind(&session.id)
            .execute(&state.db)
            .await;
        return Some(
            inject_bar(
                proxy_session(state, &session.instance, method, uri, headers, body.clone()).await,
                &config,
            )
            .await,
        );
    }

    // New visitor → claim (atomic) or wait.
    claim(state, &config, project, method, uri, headers, body.clone()).await
}

struct Session {
    id: String,
    instance: String,
    expires_at: i64,
}

async fn session_for_request(state: &AppState, headers: &HeaderMap, parent_id: &str) -> Option<Session> {
    let cookie = cookie_value(headers, DEMO_COOKIE)?;
    let hash = hex::encode(Sha256::digest(cookie.as_bytes()));
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, instance_project_id, expires_at FROM demo_sessions \
         WHERE token_hash = ? AND parent_project_id = ? AND status = 'active'",
    )
    .bind(&hash)
    .bind(parent_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    row.map(|(id, instance, expires_at)| Session { id, instance, expires_at })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let raw = value.to_str().ok()?;
        for pair in raw.split(';') {
            if let Some(v) = pair.trim().strip_prefix(&format!("{name}=")) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Claim a session: pick an unclaimed instance or spawn one, then insert the
/// session row under the per-parent lock (capacity check + insert atomic).
async fn claim(
    state: &AppState,
    config: &DemoConfig,
    parent: &Project,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Option<Response> {
    let lock = claim_lock(&parent.id).await;
    let _guard = lock.lock().await;

    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM demo_sessions WHERE parent_project_id = ? AND status = 'active'",
    )
    .bind(&parent.id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if active >= config.max_instances {
        return Some(wait_page(state, config, &parent.id).await);
    }

    // Prefer a running prewarm; a stopped one starts fast (no creation).
    // Edit/shared instances never enter the pool or the capacity count.
    let instance: Option<String> = sqlx::query_scalar(
        "SELECT p.id FROM projects p \
         WHERE p.demo_parent_id = ? AND p.status IN ('running', 'stopped') \
         AND p.id NOT IN (SELECT edit_instance_id FROM demo_config WHERE project_id = ? \
                           UNION SELECT shared_instance_id FROM demo_config WHERE project_id = ?) \
         AND NOT EXISTS (SELECT 1 FROM demo_sessions s WHERE s.instance_project_id = p.id AND s.status = 'active') \
         ORDER BY CASE p.status WHEN 'running' THEN 0 ELSE 1 END, p.created_at LIMIT 1",
    )
    .bind(&parent.id)
    .bind(&parent.id)
    .bind(&parent.id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    // Pool empty → spawn (creation runs in the background; the pending page
    // covers the wait). Bounded by max_instances overall.
    let instance = match instance {
        Some(id) => id,
        None => {
            let total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM projects p WHERE p.demo_parent_id = ? \
                 AND p.id NOT IN (SELECT edit_instance_id FROM demo_config WHERE project_id = ? \
                                   UNION SELECT shared_instance_id FROM demo_config WHERE project_id = ?)",
            )
            .bind(&parent.id)
            .bind(&parent.id)
            .bind(&parent.id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
            if total >= config.max_instances {
                return Some(wait_page(state, config, &parent.id).await);
            }
            let baseline = demo::baseline::load(state, &parent.id).await.ok();
            match demo::clone::create_instance(
                state,
                parent,
                demo::clone::CloneOpts { start: true, baseline, requested_id: None },
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(project = %parent.id, error = %e, "demo: instance spawn failed");
                    return Some(page("Demo unavailable", "Could not start a demo instance — try again shortly.", Some(15), false));
                }
            }
        }
    };

    let token = uuid::Uuid::new_v4().simple().to_string();
    let now = chrono::Utc::now().timestamp();
    let expires = now + config.session_ttl_minutes * 60;
    if let Err(e) = sqlx::query(
        "INSERT INTO demo_sessions (id, parent_project_id, instance_project_id, token_hash, created_at, expires_at, last_seen_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'active')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&parent.id)
    .bind(&instance)
    .bind(hex::encode(Sha256::digest(token.as_bytes())))
    .bind(now)
    .bind(expires)
    .bind(now)
    .execute(&state.db)
    .await
    {
        tracing::error!(project = %parent.id, error = %e, "demo: session insert failed");
        return Some(page("Demo unavailable", "Could not start a demo session — try again.", Some(10), false));
    }

    // Proxy and attach the session cookie to whatever comes back (page or
    // proxied app response alike).
    let mut resp = inject_bar(proxy_session(state, &instance, method, uri, headers, body).await, config).await;
    if let Ok(value) = header::HeaderValue::from_str(&format!(
        "{DEMO_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        config.session_ttl_minutes * 60
    )) {
        resp.headers_mut().append(header::SET_COOKIE, value);
    }
    Some(resp)
}

/// Copy of the request headers with accept-encoding removed — the bar can
/// only be injected into identity-encoded HTML.
fn identity_headers(headers: &HeaderMap) -> HeaderMap {
    let mut h = headers.clone();
    h.remove(header::ACCEPT_ENCODING);
    h
}

/// Inject the demo bottom bar into a proxied HTML response.
async fn inject_bar(resp: Response, config: &DemoConfig) -> Response {
    if !config.bar_enabled {
        return resp;
    }
    inject_html(resp, &bar_snippet(config)).await
}

/// Inject a snippet into a proxied HTML response. Non-HTML passes through
/// untouched. Hygiene: validators stripped (a 304 would serve a bar-less page
/// after a bar-ful one), CSP headers and meta-CSP tags removed, body capped
/// at 2 MiB (larger pages pass through unadorned).
async fn inject_html(resp: Response, snippet: &str) -> Response {
    let is_html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/html"));
    if !is_html {
        return resp;
    }
    // HTML over the injection cap passes through unbarred, still streamed.
    if let Some(len) = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        && len > 2 * 1024 * 1024
    {
        return resp;
    }

    let (mut parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, 2 * 1024 * 1024).await {
        Ok(b) => b,
        // Chunked body over the cap (no length to precheck) — reject, don't buffer.
        Err(_) => return (StatusCode::BAD_GATEWAY, "upstream read error").into_response(),
    };
    let mut html = String::from_utf8_lossy(&bytes).to_string();

    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);
    parts.headers.remove(header::CONTENT_LENGTH);
    for h in ["content-security-policy", "content-security-policy-report-only", "x-frame-options"] {
        parts.headers.remove(h);
    }
    strip_meta_csp(&mut html);

    let lower = html.to_ascii_lowercase();
    let at = lower.rfind("</body>").or_else(|| lower.rfind("</html>"));
    match at {
        Some(i) => html.insert_str(i, &snippet),
        None => html.push_str(&snippet),
    }

    parts.headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    // The body went through from_utf8_lossy — declare UTF-8 so the injected
    // bar text (and the page) can't be sniffed as windows-1252.
    parts.headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("text/html; charset=utf-8"));
    Response::from_parts(parts, axum::body::Body::from(html))
}
fn strip_meta_csp(html: &mut String) {
    let lower = html.to_ascii_lowercase();
    let needle = "<meta http-equiv=\"content-security-policy\"";
    let mut cut_from: Option<usize> = None;
    let mut idx = 0usize;
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_str();
    while let Some(pos) = lower[idx..].find(needle) {
        let start = idx + pos;
        let end = match lower[start..].find('>') {
            Some(g) => start + g + 1,
            None => break,
        };
        out.push_str(&bytes[idx..start]);
        idx = end;
        cut_from = Some(idx);
    }
    if cut_from.is_some() {
        out.push_str(&bytes[idx..]);
        *html = out;
    }
}

/// The bottom bar: banner (operator-configured, trusted raw HTML — links,
/// emphasis, whatever they want) + a right-side timer — a session countdown,
/// or the shared demo's next reset time.
fn bar_snippet(config: &DemoConfig) -> String {
    let banner = &config.banner_text;
    format!(
        "<style>#l8b-demo-bar{{position:fixed;left:0;right:0;bottom:0;z-index:2147483647;display:flex;justify-content:space-between;gap:12px;align-items:center;background:rgba(15,23,42,.92);color:#e2e8f0;font:12px/1.4 system-ui,sans-serif;padding:7px 14px;border-top:1px solid #334155}}#l8b-demo-bar .t{{color:#2dd4bf;font-variant-numeric:tabular-nums;white-space:nowrap}}</style>\
<div id=\"l8b-demo-bar\"><span>{banner}</span><span class=\"t\" id=\"l8b-demo-t\"></span></div>\
<script>(function(){{var el=document.getElementById('l8b-demo-t');if(!el)return;fetch('/__demo__/status',{{cache:'no-store'}}).then(function(r){{return r.json()}}).then(function(j){{if(j.resets_at){{el.textContent='resets '+new Date(j.resets_at*1000).toLocaleTimeString([],{{hour:'numeric',minute:'2-digit'}});return;}}if(!j.expires_at){{el.textContent='';return;}}var end=j.expires_at*1000;function t(){{var s=Math.max(0,Math.round((end-Date.now())/1000));el.textContent=Math.floor(s/60)+':'+String(s%60).padStart(2,'0');if(s>0)setTimeout(t,1000);else location.reload();}}t();}}).catch(function(){{}});}})();</script>"
    )
}

/// Proxy a request to a demo instance via the waker's proxy paths. A dead
/// upstream (stale running row, container still booting) falls back to the
/// pending page with a start kick — same recovery the waker uses.
async fn proxy_session(
    state: &AppState,
    instance_id: &str,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let sanitized = identity_headers(headers);
    let headers = &sanitized;
    let instance: Option<Project> = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(instance_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(instance) = instance else {
        return page("Demo session ended", "Reload to start a new one.", None, false); // torn down mid-session
    };

    // Stopped (janitor or fresh boot) → start; the pending page polls until ready.
    if !matches!(instance.status, ProjectStatus::Running | ProjectStatus::Degraded) {
        if !demo::mid_operation(&instance.status) && instance.auto_start_enabled {
            spawn_start(state, &instance.id);
        }
        return page(PENDING_TITLE, PENDING_MSG, None, true);
    }

    let is_remote = instance.node_id.as_deref().map(|n| n != "local").unwrap_or(false);
    if is_remote {
        if let Some(resp) =
            crate::routes::waker::remote_proxy_response(state, &instance, method.clone(), uri, headers, body, std::time::Duration::from_secs(2)).await
        {
            return resp;
        }
        return page(PENDING_TITLE, PENDING_MSG, None, true);
    }

    if instance.deploy_type == Some(litebin_common::types::DeployType::Compose) {
        if let Some(upstream) = crate::routes::waker::resolve_request_upstream(state, &instance, uri.path()).await {
            let resp = crate::routes::waker::proxy_request(
                &state.proxy_client,
                method.clone(),
                &upstream,
                uri.path_and_query().map(|pq| pq.as_str()),
                headers,
                body,
            )
            .await;
            if resp.status() != StatusCode::BAD_GATEWAY {
                return resp;
            }
            spawn_start(state, &instance.id);
            return page(PENDING_TITLE, PENDING_MSG, None, true);
        }
        return page(PENDING_TITLE, PENDING_MSG, None, true);
    }

    let Some(port) = instance.internal_port.map(|p| p as u16) else {
        return page("Demo unavailable", "This demo instance has no routed port.", Some(10), false);
    };
    let upstream = format!("{}:{}", litebin_common::types::container_name(&instance.id, "web", None), port);
    let resp = crate::routes::waker::proxy_with_retry(
        &state.proxy_client,
        method.clone(),
        uri,
        headers,
        body,
        &upstream,
        std::time::Duration::from_secs(2),
    )
    .await;
    if resp.status() == StatusCode::BAD_GATEWAY {
        spawn_start(state, &instance.id);
        return page(PENDING_TITLE, PENDING_MSG, None, true);
    }
    resp
}

fn spawn_start(state: &AppState, project_id: &str) {
    let state = state.clone();
    let id = project_id.to_string();
    tokio::spawn(async move {
        let _ =
            crate::routes::manage::handlers::start_project(axum::extract::State(state), axum::extract::Path(id)).await;
    });
}

/// Does the instance's app actually answer? A DB "running" row can lag a
/// slow boot or a crash-loop; "ready" must reflect a responding app.
async fn instance_answers(state: &AppState, instance: &Project) -> bool {
    // Remote instances proxy through the agent's TLS caddy — probe the same
    // way the proxy does (any HTTP response counts as up).
    let url = if instance.node_id.as_deref().map(|n| n != "local").unwrap_or(false) {
        let node_id = instance.node_id.as_deref().unwrap_or("local");
        match crate::routing_helpers::lookup_node_host(&state.db, node_id).await {
            Ok(Some((host, _))) => format!("https://{host}/"),
            _ => return false,
        }
    } else if instance.deploy_type == Some(litebin_common::types::DeployType::Compose) {
        match crate::routes::waker::resolve_request_upstream(state, instance, "/").await {
            Some(upstream) => format!("http://{}/", upstream),
            None => return false,
        }
    } else {
        match instance.internal_port.map(|p| p as u16) {
            Some(port) => format!(
                "http://{}:{}/",
                litebin_common::types::container_name(&instance.id, "web", None),
                port
            ),
            None => return false,
        }
    };
    state
        .proxy_client
        .get(&url)
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await
        .is_ok()
}

/// `GET /__demo__/status` — the pending page's poll and the bar's data
/// source. Sessions report their instance state (+ expiry for the bar
/// countdown); shared mode reports the next reset time. `ready` requires the
/// app to actually answer, not just a running DB row.
async fn status_endpoint(state: &AppState, headers: &HeaderMap, config: &DemoConfig, parent_id: &str) -> Response {
    if config.mode == "shared" {
        let resets_at = match (config.last_reset_at, config.reset_interval_minutes) {
            (Some(last), Some(interval)) => Some(last + interval * 60),
            _ => None,
        };
        // The pending page reloads on "ready" — report the shared instance's
        // actual state (also starts it if the janitor stopped it).
        let shared_project: Option<Project> = match config.shared_instance_id.as_deref() {
            Some(id) => sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
                .bind(id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let ready = match &shared_project {
            Some(p) => {
                let up = matches!(p.status, ProjectStatus::Running | ProjectStatus::Degraded)
                    && instance_answers(state, p).await;
                if !up && !demo::mid_operation(&p.status) && p.auto_start_enabled {
                    spawn_start(state, &p.id);
                }
                up
            }
            None => false,
        };
        return (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "state": if ready { "ready" } else { "pending" }, "resets_at": resets_at })),
        )
            .into_response();
    }
    let state_json = match session_for_request(state, headers, parent_id).await {
        None => serde_json::json!({ "state": "none" }),
        Some(session) if session.expires_at <= chrono::Utc::now().timestamp() => {
            serde_json::json!({ "state": "expired" })
        }
        Some(session) => {
            let instance: Option<Project> = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
                .bind(&session.instance)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
            match instance.as_ref().map(|p| &p.status) {
                Some(s) if matches!(s, ProjectStatus::Running | ProjectStatus::Degraded) => {
                    let ready = instance_answers(state, instance.as_ref().unwrap()).await;
                    serde_json::json!({ "state": if ready { "ready" } else { "pending" }, "expires_at": session.expires_at })
                }
                Some(ProjectStatus::Stopped) => {
                    spawn_start(state, &session.instance);
                    serde_json::json!({ "state": "pending", "expires_at": session.expires_at })
                }
                Some(_) => serde_json::json!({ "state": "pending", "expires_at": session.expires_at }),
                None => serde_json::json!({ "state": "error" }),
            }
        }
    };
    (StatusCode::OK, axum::Json(state_json)).into_response()
}

async fn redirect_to_demo_host(state: &AppState, parent_id: &str) -> Response {
    let parent: Option<Project> = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(parent_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let host = parent
        .and_then(|p| p.custom_domain.clone())
        .unwrap_or_else(|| format!("{parent_id}.{}", state.platform.domain()));
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, format!("https://{host}/"))
        .body(axum::body::Body::from("demo gateway"))
        .unwrap()
}

async fn wait_page(state: &AppState, config: &DemoConfig, parent_id: &str) -> Response {
    let earliest: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(expires_at) FROM demo_sessions WHERE parent_project_id = ? AND status = 'active'",
    )
    .bind(parent_id)
    .fetch_one(&state.db)
    .await
    .ok()
    .flatten();
    let in_x = earliest
        .map(|e| ((e - chrono::Utc::now().timestamp()).max(0) / 60).max(1))
        .map(|mins| format!("the next one frees within ~{mins} min"))
        .unwrap_or_else(|| "one frees up soon".into());
    page(
        "Demo capacity reached",
        &format!("All {} demo sessions are in use — {}. Retrying automatically…", config.max_instances, in_x),
        Some(30),
        false,
    )
}

/// Shared themed fringe page. `refresh_secs` = meta-refresh interval (pages
/// that poll instead pass None); `spinner` adds the loading ring.
fn page(title: &str, message: &str, refresh_secs: Option<u32>, spinner: bool) -> Response {
    let refresh_tag = refresh_tag(refresh_secs);
    let poll = if refresh_secs.is_none() {
        // Pending/expired pages poll the gatekeeper status endpoint.
        "<script>setInterval(async()=>{try{const r=await fetch('/__demo__/status',{cache:'no-store'});const j=await r.json();if(j.state==='ready'||j.state==='none')location.reload();}catch(e){}},1500)</script>"
    } else {
        ""
    };
    let spinner_html = if spinner { "<div class=\"ring\"></div>" } else { "" };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} — Demo</title>{refresh_tag}<style>\
         *{{box-sizing:border-box}}\
         body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         background:radial-gradient(1200px 600px at 50% -10%,#1e293b 0%,#0b1120 60%);\
         color:#e2e8f0;font:14px/1.6 system-ui,-apple-system,sans-serif}}\
         .card{{max-width:420px;margin:24px;text-align:center;background:rgba(15,23,42,.82);\
         border:1px solid rgba(51,65,85,.6);border-radius:16px;padding:36px 32px;\
         box-shadow:0 20px 60px rgba(0,0,0,.45);backdrop-filter:blur(6px)}}\
         .ring{{width:34px;height:34px;margin:0 auto 18px;border-radius:50%;\
         border:3px solid #334155;border-top-color:#8b5cf6;animation:spin 1s linear infinite}}\
         @keyframes spin{{to{{transform:rotate(360deg)}}}}\
         h1{{font-size:17px;font-weight:600;margin:0 0 8px;color:#f1f5f9}}\
         p{{margin:0;color:#94a3b8;font-size:13.5px}}</style></head>\
         <body><div class=\"card\">{spinner_html}<h1>{title}</h1><p>{message}</p></div>{poll}</body></html>"
    );
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
        html,
    )
        .into_response()
}

fn refresh_tag(secs: Option<u32>) -> String {
    secs.map(|s| format!("<meta http-equiv=\"refresh\" content=\"{s}\">")).unwrap_or_default()
}
