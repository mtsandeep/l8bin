const CSS: &str = r##"<style>
*{margin:0}.wp-body{min-height:100vh;display:flex;align-items:center;justify-content:center;background:#0f172a;color:#e2e8f0;font-family:system-ui}.wp-card{text-align:center;padding:24px}.wp-mark{width:72px;height:72px;margin:0 auto 20px}.wp-mark svg{width:100%;height:100%;overflow:visible}.wp-layer{transform-box:fill-box}.wp-layer.b1{--d:20px;animation:wp-rise 1.9s cubic-bezier(.45,0,.25,1) infinite}.wp-layer.b2{--d:26px;animation:wp-rise 1.9s cubic-bezier(.45,0,.25,1) .18s infinite}.wp-layer.b3{--d:34px;animation:wp-rise 1.9s cubic-bezier(.45,0,.25,1) .36s infinite}@keyframes wp-rise{0%,100%{transform:translateY(var(--d))}35%,70%{transform:translateY(0)}}.wp-bin{transform-box:fill-box;animation:wp-bin 1.9s ease-in-out infinite}@keyframes wp-bin{0%,100%{opacity:1}50%{opacity:.92}}.wp-title{font-size:1.05rem;font-weight:600;margin:0 0 4px}.wp-sub{color:#64748b;font-size:.8rem;margin:0}.wp-status{height:72px;margin-top:10px;display:flex;flex-direction:column;align-items:center;justify-content:flex-start;gap:10px}.wp-status .wp-sub{height:22px;line-height:22px}.wp-progress{width:230px;height:3px;background:#1e293b;border-radius:99px;overflow:hidden}.wp-progress i{display:block;height:100%;width:0;background:linear-gradient(90deg,#64748b,#94a3b8);border-radius:99px;animation:wp-fill 14s cubic-bezier(.22,1,.36,1) forwards}@keyframes wp-fill{0%{width:4%}12%{width:42%}30%{width:58%}55%{width:70%}100%{width:92%}}.wp-phases{height:22px;position:relative;width:100%}.wp-phases span{position:absolute;inset:0;opacity:0;font-size:.9rem;color:#94a3b8;animation:wp-cycle 10.4s linear infinite}.wp-phases span:nth-child(2){animation-delay:2.6s}.wp-phases span:nth-child(3){animation-delay:5.2s}.wp-phases span:nth-child(4){animation-delay:7.8s}@keyframes wp-cycle{0%{opacity:0;transform:translateY(4px)}4%,21%{opacity:1;transform:translateY(0)}25%,100%{opacity:0;transform:translateY(-4px)}}.wp-progress.shimmer i{width:100%;background:linear-gradient(90deg,#1e293b 0%,#94a3b8 50%,#1e293b 100%);background-size:200% 100%;animation:wp-shimmer 2s linear infinite}@keyframes wp-shimmer{0%{background-position:200% 0}100%{background-position:-200% 0}}.wp-emoji-line{color:#f87171;font-size:.9rem}.wp-footer{position:fixed;bottom:16px;left:0;right:0;text-align:center;color:#94a3b8;font-size:.85rem}.wp-footer a{color:inherit;text-decoration:none}.wp-footer .wp-brand{border-bottom:1px dashed #475569}.wp-body.danger .wp-layer{animation:wp-emerge .5s cubic-bezier(.2,.7,.3,1) both}.wp-body.danger .wp-layer.b2{animation-delay:.12s}.wp-body.danger .wp-layer.b3{transform-origin:0 100%;animation:wp-emerge .5s cubic-bezier(.2,.7,.3,1) .24s both,wp-fall .45s cubic-bezier(.35,.7,.45,1) 1s forwards}.wp-body.danger .wp-bin{animation:none}@keyframes wp-emerge{0%{transform:translateY(var(--d))}100%{transform:translateY(0)}}@keyframes wp-fall{0%{transform:rotate(0);fill:#06b6d4}55%{transform:rotate(-45deg)}100%{transform:rotate(-38deg);fill:#f87171}}.wp-body.failed .wp-layer.b1,.wp-body.failed .wp-layer.b2,.wp-body.failed .wp-bin{animation:none}.wp-body.failed .wp-layer.b3{fill:#f87171;transform-origin:0 100%;animation:wp-fall .45s cubic-bezier(.35,.7,.45,1) forwards}.wp-body.quiet .wp-layer,.wp-body.quiet .wp-bin{animation:none}.wp-body.quiet text{animation:wp-q .8s cubic-bezier(.2,.7,.3,1) .25s both}@keyframes wp-q{0%{transform:translateY(34px)}100%{transform:translateY(0)}}.wp-body.offline .wp-layer{animation:none;fill:#64748b;opacity:1}.wp-body.offline .wp-bin{animation:none;fill:#475569}.wp-body.offline .wp-title,.wp-body.offline .wp-sub{color:#64748b}
</style>"##;

const MARK_LIVE: &str = r##"<svg viewBox="22 14 52 70" aria-hidden="true"><rect class="wp-layer b1" x="40" y="32" width="6" height="12" rx="2" fill="#06b6d4" opacity="0.55"/><rect class="wp-layer b2" x="54" y="24" width="6" height="16" rx="2" fill="#06b6d4" opacity="0.8"/><rect class="wp-layer b3" x="68" y="14" width="6" height="20" rx="2" fill="#06b6d4"/><rect class="wp-bin" x="22" y="48" width="52" height="36" rx="3" fill="#8b5cf6"/></svg>"##;

const MARK_QUESTION: &str = r##"<svg viewBox="22 14 52 70" aria-hidden="true"><text x="57" y="41" text-anchor="middle" font-family="system-ui,sans-serif" font-size="36" font-weight="700" fill="#06b6d4">?</text><rect class="wp-bin" x="22" y="48" width="52" height="36" rx="3" fill="#8b5cf6"/></svg>"##;

const MARK_DASHED: &str = r##"<svg viewBox="22 14 52 70" aria-hidden="true"><rect x="22" y="48" width="52" height="36" rx="3" fill="none" stroke="#8b5cf6" stroke-width="3" stroke-dasharray="6 5" stroke-linecap="round"/></svg>"##;

const FOOTER: &str = r#"<footer class="wp-footer">Powered by <a href="https://l8bin.com"><span class="wp-brand">L8Bin.com</span></a></footer>"#;

const WAIT_JS: &str = r#"<script>
(function(){var w=true,b=document.querySelector('.wp-progress i');
console.info('[l8bin] waking project: '+(document.body.getAttribute('data-p')||''));
function fail(){w=false;document.body.classList.add('failed');document.querySelector('.wp-title').textContent='Failed to start the website';document.querySelector('.wp-phases').style.display='none';document.querySelector('.wp-progress').style.display='none';document.getElementById('errmsg').style.display='block';setTimeout(function(){location.reload()},30000)}
function ok(){w=false;if(b){b.style.animation='none';b.style.width='100%'}setTimeout(function(){location.reload()},180)}
function poll(){if(!w)return;var c=typeof AbortController!=='undefined'?new AbortController():null,t=c&&setTimeout(function(){c.abort()},10000),o={headers:{'Accept':'application/json'},cache:'no-store'};if(c)o.signal=c.signal;fetch('/__l8b_wake_wait',o).then(function(r){clearTimeout(t);if(r.headers.get('x-l8b-wake')){return r.json().then(function(d){if(!w)return;if(d&&d.status==='ready'){ok();return}if(d&&d.status==='failed'){fail();return}setTimeout(poll,250)}).catch(function(){setTimeout(poll,1000)})}else if(r.status===502||r.status===503||r.status===504){setTimeout(poll,1000)}else{ok()}}).catch(function(){clearTimeout(t);setTimeout(poll,1000)})}
poll()})();
</script>"#;

fn page(
    body_class: &str,
    data_p: Option<&str>,
    title: &str,
    mark: &str,
    status: &str,
    head_extra: &str,
    script: &str,
) -> String {
    let body_attr = match data_p {
        Some(p) => format!(r#" class="{}" data-p="{}""#, body_class, p),
        None => format!(r#" class="{}""#, body_class),
    };
    format!(
        concat!(
            "<!DOCTYPE html><html><head><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
            "<meta name=\"robots\" content=\"noindex\"><title>{title}</title>{css}{head}</head>",
            "<body{body}><main class=\"wp-card\"><div class=\"wp-mark\">{mark}</div>",
            "<h1 class=\"wp-title\">{title}</h1><div class=\"wp-status\">{status}</div></main>{footer}{script}</body></html>"
        ),
        title = title,
        css = CSS,
        head = head_extra,
        body = body_attr,
        mark = mark,
        status = status,
        footer = FOOTER,
        script = script,
    )
}

pub fn loading_page_html(name: &str, action: &str) -> String {
    let log = format!("<script>console.info('[l8bin] {act} project: {name}')</script>", act = action.to_lowercase());
    page(
        "wp-body",
        None,
        "Updating the website",
        MARK_LIVE,
        r#"<p class="wp-sub">this takes a moment — the page refreshes itself</p><div class="wp-progress shimmer"><i></i></div>"#,
        r#"<meta http-equiv="refresh" content="2">"#,
        &log,
    )
}

pub fn waiting_page_html(name: &str) -> String {
    page(
        "wp-body",
        Some(name),
        "Getting the website ready",
        MARK_LIVE,
        concat!(
            r#"<div class="wp-phases">"#,
            r#"<span>getting things ready…</span><span>warming things up…</span>"#,
            r#"<span>just a little longer…</span><span>almost there…</span></div>"#,
            r#"<div class="wp-progress"><i></i></div>"#,
            r#"<div id="errmsg" style="display:none"><p class="wp-sub">the website didn't respond in time — it may still be booting</p><div class="wp-emoji-line">Retrying in 30 seconds…</div></div>"#,
        ),
        r#"<noscript><meta http-equiv="refresh" content="1"></noscript>"#,
        WAIT_JS,
    )
}

pub fn error_page_html() -> String {
    page(
        "wp-body danger",
        None,
        "Failed to start the website",
        MARK_LIVE,
        r#"<p class="wp-sub">the website didn't respond in time — it may still be booting</p><div class="wp-emoji-line">Retrying in 30 seconds…</div>"#,
        r#"<meta http-equiv="refresh" content="30">"#,
        "",
    )
}

pub fn offline_page_html() -> String {
    page(
        "wp-body offline",
        None,
        "This website is currently offline",
        MARK_LIVE,
        r#"<p class="wp-sub">auto-start is disabled — the owner can enable it from the dashboard</p>"#,
        "",
        "",
    )
}

pub fn not_found_page_html() -> String {
    page(
        "wp-body quiet",
        None,
        "Website not found",
        MARK_QUESTION,
        r#"<p class="wp-sub">this website does not exist or has been removed</p>"#,
        "",
        "",
    )
}

pub fn not_ready_page_html() -> String {
    page(
        "wp-body quiet",
        None,
        "Website not ready!",
        MARK_DASHED,
        r#"<p class="wp-sub">complete the deployment to have your website live</p>"#,
        "",
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_page_contract() {
        let html = waiting_page_html("myapp");
        assert!(html.contains("/__l8b_wake_wait"));
        assert!(html.contains("x-l8b-wake"));
        assert!(html.contains("AbortController"));
        assert!(html.contains(r#"content="noindex""#));
        assert!(html.contains(r#"<noscript><meta http-equiv="refresh" content="1"></noscript>"#));
        assert_eq!(html.matches("http-equiv=\"refresh\"").count(), 1);
        assert!(html.contains(r#"data-p="myapp""#));
        assert!(html.contains("Getting the website ready"));
        assert!(!html.contains("Starting myapp"));
    }

    #[test]
    fn every_page_is_self_contained_and_noindexed() {
        for html in [
            loading_page_html("x", "Deploying"),
            waiting_page_html("x"),
            error_page_html(),
            offline_page_html(),
            not_found_page_html(),
            not_ready_page_html(),
        ] {
            assert!(html.contains(r#"content="noindex""#));
            assert!(html.contains("<style>"));
            assert!(!html.contains("<link") && !html.contains("src=") && !html.contains("@import"));
            assert!(html.contains("wp-footer"));
        }
    }

    #[test]
    fn not_found_uses_question_mark() {
        assert!(not_found_page_html().contains(">?</text>"));
    }

    #[test]
    fn loading_is_neutral_in_ui_real_action_in_console() {
        let html = loading_page_html("myapp", "Stopping");
        assert!(html.contains("Updating the website"));
        assert!(!html.contains("Stopping myapp"));
        assert!(html.contains("[l8bin] stopping project: myapp"));
        assert!(html.contains(r#"<meta http-equiv="refresh" content="2">"#));
    }
}
