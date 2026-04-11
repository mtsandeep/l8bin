# TODO

## 2026-04-11 — SEO: Loading page returns 200 OK

The auto-wake loading page returns HTTP 200 — Googlebot may index the spinner page as site content.

**Fix:** Return `503 Service Unavailable` with a `Retry-After` header instead. Googlebot won't index 503 pages and will retry later.

- `orchestrator/src/routes/waker.rs` — `loading_page_html()`
- `agent/src/routes/waker.rs` — `loading_page()`
