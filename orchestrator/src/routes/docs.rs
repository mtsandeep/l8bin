use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};

pub async fn serve_docs() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        HTML,
    )
}

const HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>LiteBin API Docs</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
html, body { height: 100%; }
body { background: #0f0f14; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; }
</style>
</head>
<body>
<div id="app"></div>
<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
<script>
Scalar.createApiReference('#app', {
  url: '/openapi.json',
  theme: 'purple',
  darkMode: true,
  layout: 'modern',
  customCss: `.scalar-app { --scalar-color-1: #7c3aed; --scalar-background: #0f0f14; }`
});
</script>
</body>
</html>"#;
