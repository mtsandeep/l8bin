/// Shared HTML templates for waker pages (loading, error, not-found, offline).
/// Both the orchestrator and agent wakers use these — the HTML/CSS is identical,
/// only the response wrapping differs per framework.

pub fn footer_html() -> String {
    format!(
        r#"<footer style="position:fixed;bottom:16px;left:0;right:0;text-align:center;color:#94a3b8;font-size:1rem;">Powered by <a href="https://l8bin.com" style="color:#7c3aed;text-decoration:none;">l8bin</a></footer>"#
    )
}

/// "Starting {name}..." page with spinner, auto-refreshes every 1 second.
pub fn loading_page_html(name: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta http-equiv="refresh" content="1">
    <title>Starting {name}</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .loader {{ text-align: center; }}
        .spinner {{ width: 40px; height: 40px; border: 4px solid #334155; border-top: 4px solid #38bdf8; border-radius: 50%; animation: spin 1s linear infinite; margin: 0 auto 16px; }}
        @keyframes spin {{ 0% {{ transform: rotate(0deg); }} 100% {{ transform: rotate(360deg); }} }}
    </style>
</head>
<body>
    <div class="loader">
        <div class="spinner"></div>
        <p>Starting <strong>{name}</strong>...</p>
        {footer}
    </div>
</body>
</html>"#,
        name = name,
        footer = footer_html(),
    )
}

/// "Failed to start the website" page, auto-refreshes every 30 seconds.
pub fn error_page_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta http-equiv="refresh" content="30">
    <title>Offline</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .msg {{ text-align: center; }}
        h2 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }}
        p {{ color: #64748b; margin: 0; font-size: 0.875rem; }}
    </style>
</head>
<body>
    <div class="msg">
        <h2>Failed to start the website</h2>
        <p>Retrying in 30 seconds...</p>
        {footer}
    </div>
</body>
</html>"#,
        footer = footer_html(),
    )
}

/// "Project not found" page (no auto-refresh).
pub fn not_found_page_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Not Found</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .msg {{ text-align: center; }}
        h2 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }}
        p {{ color: #64748b; margin: 0; font-size: 0.875rem; }}
    </style>
</head>
<body>
    <div class="msg">
        <h2>Project not found</h2>
        <p>This project does not exist or has been removed.</p>
        {footer}
    </div>
</body>
</html>"#,
        footer = footer_html(),
    )
}

/// "This website is currently offline" page (auto-start disabled).
pub fn offline_page_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Offline</title>
    <style>
        body {{ font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0f172a; color: #e2e8f0; }}
        .msg {{ text-align: center; }}
        h2 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 8px; }}
        p {{ color: #64748b; margin: 0; font-size: 0.875rem; }}
    </style>
</head>
<body>
    <div class="msg">
        <h2>This website is currently offline</h2>
        <p>Auto-start is disabled!</p>
        {footer}
    </div>
</body>
</html>"#,
        footer = footer_html(),
    )
}
