# Codebase Hardening (Small Items)

## Context

Small code-level items left over from the maintainability review — each too small for its own effort, collected here. None are bugs today; all are seams that make future work harder than it should be. Pick items off opportunistically; retire each row when done.

## Items

| # | Item | Where | Why |
|---|---|---|---|
| 1 | Clock seam: replace ~50 inline `chrono::Utc::now().timestamp()` calls with an injectable clock (fn pointer or trait on AppState/AgentState) | `orchestrator/src/sleep/janitor.rs`, `nodes/heartbeat.rs`, `routes/` timestamp binds; agent `agent_auth` freshness already central | Time-dependent logic (janitor idle timeouts, HMAC freshness windows, last_active_at) becomes unit-testable |
| 2 | Retry/backoff helper for transient agent HTTP calls | `litebin-common` (next to `agent_auth`) | Callers currently do one-shot requests; a blip mid-deploy surfaces as a user-facing 503 instead of a retry |
| 3 | Shared `reqwest::Client` in agent | `agent/src/routes/waker/handler.rs`, `activity.rs`, `multi_service.rs` build ad-hoc clients | Connection reuse + one place for timeouts |
| 4 | Deduplicate `default_false()` | `agent/src/routes/containers/types.rs` vs `batch_run/types.rs` (identical private fns) | One-line cleanup; also share as `pub(crate)` serde default |
| 5 | Release builds with `--locked` | `.github/workflows/release.yml` | Release currently builds with whatever transitive deps resolve at tag time, not what CI tested |
| 6 | Dependabot/Renovate config | `.github/dependabot.yml` | Lockfile is committed now; automated bump PRs run the full CI gate (cargo + pnpm for dashboard) |
| 7 | `ProjectStatus` SQL literal purge | covered in `query-safety.md` step 1 | Listed here only as a cross-reference |
| 8 | CLI `PublicStats`-style drift check | none remaining — kept that way via the typed agent API ([agent-api.md](../agent-api.md)) | Cross-reference |

## Considerations

- Items 1–2 interact: the retry helper should take the clock seam, so tests can fast-forward backoff.
- Item 5 is a one-word change per build line; do it with the next release.
- Item 6 config should group updates (weekly cargo batch + weekly pnpm batch) to avoid PR spam.

## Priority

Low individually; items 1, 2, and 5 are the ones that pay back soonest.
