# Engineering Debt

Internal code-quality work — no user-facing behavior. Consolidated from the former error-types, hot-function-extraction, query-safety, codebase-hardening, and test-coverage notes.

**Last verified: 2026-10-20**

| Item | Status |
|---|---|
| Correctness: deploy-lock race, janitor leak, lock poisoning | Open |
| Typed API errors | Not started |
| Split run_service_container / start_services | Deferred — take when touching those files |
| SQL status-literal safety | Not started |
| Small code-quality items | Partial |
| Agent + CLI test coverage | Partial |

---

## Correctness / reliability fixes

| Item | Where | What |
|---|---|---|
| **Deploy lock dropped before spawn** | `deploy/single/core.rs` `execute_deploy`, `deploy/compose/persist.rs` | permit is a handler local; the heavy work spawns without it, so the waker's lock check sees "free" and races container mutations. Move the permit into the spawned task. |
| **Janitor leaks containers** | `sleep/janitor.rs` | flips status to Stopped before stopping containers; mid-sweep `?` aborts strand already-flipped projects (candidate query only matches `running`). Isolate per-project errors; stop before/with the flip. Also reconcile stale `stopping` rows at boot. |
| **Lock poisoning** | agent `project_meta` / `registration` / `last_caddy_config`, orchestrator `platform` / `destination_health` | std locks with `.unwrap()` (~27 sites) — one panic under a write lock 500s every request until restart. Swap to `parking_lot` (no poisoning). |
| Swallowed writes diverge state | `routes/settings.rs` (service limit sync), `github/project_link.rs` (superseded-token deletes — security-adjacent), agent `multi_service.rs` network connects, `recreate.rs`, `deploy/compose/local.rs` | `let _ =` on DB/status writes. Log at minimum; decide case-by-case which need real failure handling. |
| Waker hot path | agent `caddy.rs` / `wake.rs`, orchestrator `waker/helpers.rs` | sync fs reads + full compose re-parse per request; new mTLS client per visitor request. Cache the parsed plan (mtime key), pool the proxy client. |
| Boot sequence duplication | orchestrator/agent `main.rs` | ~120 duplicated lines; orchestrator's upload-GC ignores shutdown and races `db.close()`. Extract `litebin-common::boot`. |
| Token mint ×4 + weak agent secret | `deploy_tokens.rs`, `github/mod.rs`, `device_auth.rs`, `upload.rs`; `routes/nodes/mod.rs` | one `mint_token()` in litebin-common; agent HMAC secret is `sha256(uuid)` (~122 bits) — generate from 32 random bytes. |

---

## Typed API errors (anyhow → thiserror + ApiError)

**Status: NOT STARTED**

### Context

All crate-internal errors use `anyhow::Error` with `bollard::Error` and `sqlx::Error` erased via `.into()`. Callers that need to distinguish error cases (e.g., 404 vs 500) must downcast or string-match. String-matching is already gone, but the underlying types are still erased.

Converting key error paths to `thiserror` enums gives callers typed match arms instead of downcasting.

### Current Pattern (to replace)

```rust
// DockerManager methods — all return anyhow
pub async fn stop_container(&self, id: &str) -> anyhow::Result<()> { ... }

// Callers classify errors via downcast
match DockerErrorKind::from_anyhow(&e) {
    DockerErrorKind::NotFound => ...,
    _ => ...,
}
```

### Target Pattern

```rust
##[derive(thiserror::Error, Debug)]
pub enum DockerError {
    #[error("container not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("docker API error: {0}")]
    Api(#[from] bollard::errors::Error),
    #[error("docker connection error: {0}")]
    Connection(#[from] std::io::Error),
}

// Methods return typed errors
pub async fn stop_container(&self, id: &str) -> Result<(), DockerError> { ... }

// Callers match directly
match docker.stop_container(id).await {
    Err(DockerError::NotFound(_)) => ...,
    Err(e) => ...,
}
```

### Scope

| Error type | File(s) | Impact |
|---|---|---|
| `DockerError` | `litebin-common/src/docker/` | All `DockerManager` methods |
| `DbError` | `orchestrator/src/` | SQL queries + UNIQUE constraint detection |
| `CloudflareError` | `litebin-common/src/cloudflare.rs` | DNS API calls |

### Considerations

- `DockerManager` methods are called from axum handlers that return `impl IntoResponse` — these already use `match` for error mapping, so the change is mostly mechanical.
- `DbError` would replace `is_unique_constraint()` helper in `validation.rs` with `DbError::UniqueConstraint` variant.
- `CloudflareError` would replace the `code == Some(81057)` check with `CloudflareError::DuplicateRecord`.
- The `orchestrator` → `agent` HTTP calls use `reqwest` — these already have a minimal typed error (`AgentClientError` in `orchestrator/src/nodes/client.rs`, see [agent-api.md](../dev/development.md)). This effort should fold it into the fuller `ClientError` design.
- Callers outside the workspace (none currently) would need the error types re-exported from `litebin-common`.
- Beyond the internal errors above, ~60 orchestrator handler files return `(StatusCode, String)` — the same effort should introduce one `ApiError` enum with a central `IntoResponse` impl so error response shapes are consistent for the dashboard. Agent side: every Docker failure flattens to an opaque 500 (`lifecycle.rs`), NotFound→200 exists in exactly one handler, the same failure maps to 500/502/503 depending on the handler, and `ComposeError` is erased to anyhow in `compose_run.rs` (an unparseable compose silently reads as "no public service" in caddy route rebuild).

### Priority

Medium — still code quality / ergonomics only (no behavioral changes), but it unlocks consistent dashboard error handling across the `(StatusCode, String)` handler signatures. The wire-contract dependency is resolved (see [agent-api.md](../dev/development.md)).

---

## Split run_service_container / start_services

**Status: DEFERRED — take when touching these files anyway**

### Context

The file-size refactor left exactly two oversized functions — the two highest-risk units in the codebase, both requiring ~400 lines of context to change safely:

| Function | Location | Size | Why it's risky |
|---|---|---|---|
| `run_service_container` | `litebin-common/src/docker/container/run.rs` | ~390 lines | Interleaves port bindings, raw-ports policy, socket sanitization, security overrides, compose-vs-single create paths, startup stabilization, mapped-port resolution — all in one body with one shared closure |
| `start_services` level loop | `orchestrator/src/routes/manage/multi_service/start.rs` | ~470 lines | Level-by-level JoinSet scheduling, per-service fast-path/recreate task, proxy loopback verification, **four** near-identical rollback blocks, DOCKER_HOST rewrite |

Unlike the round-2 extractions (which moved inline blocks of *sequential* handler code), these are stateful: the loop bodies mutate shared Arc<Mutex<...>> trackers and the closure captures seven locals. Extraction means threading that state through a small context struct per function.

### Target Pattern

For `run_service_container`: split along its natural seams into helpers on `DockerManager` or free fns in `run.rs` —
- `compute_port_bindings(&RunServiceConfig) -> Result<PortPlan>` (public port, proxy loopback, raw ports, reserved-port policy)
- `apply_security_overrides(host: &mut HostConfig, limits: ResourceLimits)`
- `build_compose_create_body(...)` / `build_single_create_body(...)` (the two paths share only the overrides)
- `resolve_mapped_port(...)` (the non-fatal inspect-and-warn tail)

For `start_services`: extract the per-service task closure into `start_one_service(...)`, and unify the three rollback blocks (proxy-inspect failure, task failure, panic) into one `rollback_and_mark(...)` helper parameterized by whether to stop containers first — they are currently copy-pasted with tiny differences, which is itself a latent bug surface.

### Considerations

- Behavior-preserving, but concurrency code — do it under the existing test suite plus the agent live tests (`cargo test -p litebin-agent -- --ignored`) run before and after.
- The rollback unification in `start_services` is the highest-value piece alone: today the three copies have already drifted subtly (one logs "rollback: stopped after failure", two don't).
- Do the two functions as separate efforts; `run_service_container` first (fewer moving parts, pure construction logic dominates).

### Priority

Medium — no current pain, but every future feature touching ports, security policy, or orchestration lands in these two bodies.

---

## SQL status-literal safety

**Status: NOT STARTED**

### Context

336 `sqlx::query` calls across the orchestrator, all with inline SQL strings. Two classes of silent failure are possible today:

1. **Typo'd SQL or column names** — caught only at runtime, per call site.
2. **Hand-written status literals** — 37 SQL statements embed strings like `'running'`, `'deploying'`, `'stopping'` in WHERE/CASE clauses while a `ProjectStatus` enum exists. A typo (`'runing'`) compiles, matches nothing, and fails silently.

### Current Pattern

```rust
// literal duplicated between SQL and the enum's Display
"SELECT ... WHERE status = 'running' AND auto_stop_enabled = 1"
sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM projects WHERE id = ?")
```

### Target Pattern

**Step 1 (cheap, do first):** every status literal in SQL goes through the enum — either bind `ProjectStatus::Running.to_string()` as a parameter instead of inlining, or `impl ProjectStatus { pub const fn as_str(&self) -> &'static str }` and use named constants in SQL. Zero behavior change; grep-auditable (`grep "'running'"` must return nothing).

**Step 2 (optional, larger):** adopt sqlx compile-time checked macros (`query!` / `query_as!`) for the orchestrator. Requires a `DATABASE_URL` (or offline `sqlx prepare` cache committed for CI) pointed at a migrated SQLite file. Catches column/typo/type errors at build time across all 336 call sites.

### Scope

| Piece | Notes |
|---|---|
| Status-literal purge | ~37 statements; mechanical |
| `ProjectStatus::as_str` | litebin-common `types.rs`; replaces ad-hoc `to_string()` in SQL binds too |
| sqlx offline cache (step 2) | `.sqlx/` directory committed; CI already has `--locked`-style discipline to extend |

### Considerations

- Step 2 changes every call site's macro name; do it per-module over time, not big-bang.
- Some queries build dynamic SQL (QueryBuilder in `routes/heartbeat.rs`, stats) — those stay runtime-checked by nature.
- SQLite schema lives in embedded migrations (`sqlx::migrate!`), so the prepare cache must be regenerated when migrations change — add a CI check that `cargo sqlx prepare --check` passes.

### Priority

Low-medium — step 1 is cheap and worth doing opportunistically; step 2 only when touching SQL-heavy code anyway.

---

## Small code-quality items

**Status: PARTIAL — pick items off opportunistically; retire each row when done**

### Context

Small code-level items, each too small for its own effort, collected here. None are bugs today; all are seams that make future work harder than it should be.

### Items

| # | Item | Where | Why |
|---|---|---|---|
| 1 | Clock seam: replace ~50 inline `chrono::Utc::now().timestamp()` calls with an injectable clock (fn pointer or trait on AppState/AgentState) | `orchestrator/src/sleep/janitor.rs`, `nodes/heartbeat.rs`, `routes/` timestamp binds; agent `agent_auth` freshness already central | Time-dependent logic (janitor idle timeouts, HMAC freshness windows, last_active_at) becomes unit-testable |
| 2 | Retry/backoff helper for transient agent HTTP calls | `litebin-common` (next to `agent_auth`) | Callers currently do one-shot requests; a blip mid-deploy surfaces as a user-facing 503 instead of a retry |
| 3 | Shared `reqwest::Client` in agent | `agent/src/routes/waker/handler.rs`, `activity.rs`, `multi_service.rs` build ad-hoc clients | Connection reuse + one place for timeouts |
| 4 | Deduplicate `default_false()` | `agent/src/routes/containers/types.rs` vs `batch_run/types.rs` (identical private fns) | One-line cleanup; also share as `pub(crate)` serde default |
| 5 | Release builds with `--locked` | `.github/workflows/release.yml` | Release currently builds with whatever transitive deps resolve at tag time, not what CI tested |
| 6 | Dependabot/Renovate config | `.github/dependabot.yml` | Lockfile is committed now; automated bump PRs run the full CI gate (cargo + pnpm for dashboard) |
| 7 | `ProjectStatus` SQL literal purge | covered in `query-safety.md` step 1 | Listed here only as a cross-reference |
| 8 | CLI `PublicStats`-style drift check | none remaining — kept that way via the typed agent API ([agent-api.md](../dev/development.md)) | Cross-reference |
| 9 | Delete dead exports: `types.rs` `project_data_dir`, `proxy.rs` `wants_json` | litebin-common | Zero workspace callers |
| 10 | Add `[workspace.dependencies]`; align `thiserror` 1→2 | workspace `Cargo.toml`s | Versions re-declared per crate; skew already happened |
| 11 | `spawn_log_watcher` has no shutdown/backoff | `litebin-common/src/docker/container/sidecar.rs` | Reconnects every 10s forever after receiver drops; bounded variant exists in `heartbeat.rs` |
| 12 | Rate limiter never evicts; no-XFF requests exempt | `orchestrator/src/rate_limit.rs` | DashMap grows per unique IP for process lifetime |
| 13 | Deploy log buffers uncapped; cleared only on success | `orchestrator/src/routes/deploy/logs.rs` | Failed deploys leak their buffer until next success |
| 14 | Env snapshot `lines().skip(5)` header coupling; `file_hash` = 0 on missing file | `agent/src/routes/containers/env.rs` | Header edit recreates every project on wake; missing `.env` ≡ missing snapshot |
| 15 | Proxy retries replay non-idempotent bodies; `"litebin-agent:8444"` literal ×9 | waker proxy loops; `agent/caddy.rs` + `cloudflare_router/caddy_config.rs` | POST double-submit on 502; constant exists (`AGENT_WAKE_UPSTREAM`) but unused there |
| 16 | Brittle guarded unwraps in `routes/settings.rs` (flag-then-`unwrap` ×13) | orchestrator settings handlers | One reordered early-return turns them live |

### Considerations

- Items 1–2 interact: the retry helper should take the clock seam, so tests can fast-forward backoff.
- Item 5 is a one-word change per build line; do it with the next release.
- Item 6 config should group updates (weekly cargo batch + weekly pnpm batch) to avoid PR spam.

### Priority

Low individually; items 1, 2, and 5 are the ones that pay back soonest.

---

## Agent + CLI test coverage

**Status: PARTIAL (CLI adequate; agent still thin)**

### Context

The orchestrator has a real integration suite (178 tests, in-memory SQLite + real migrations). The other crates:

| Crate | Tests | State |
|---|---|---|
| orchestrator | 178 | Integration + unit; healthy |
| compose-bollard | 53 | Unit + proptest; healthy |
| litebin-common | 87 | Unit + proptest; healthy |
| agent | 5 | Still the gap — owns container lifecycle (waker, Caddy surgery, rollback) with essentially no CI coverage; highest-value targets are the pure seams in the waker path and caddy route-merge |
| cli | 50 | Adequate |

The agent's deploy path (`batch_run/prepare.rs`, `execute.rs`) contains the highest-risk pure logic in that crate — proxy reuse/replacement decisions, host-network authorization, partial-redeploy targeting, resource-override application — with zero unit coverage; the only tests need a real Docker engine. The CLI ships easily-testable pure logic (public-service candidates, env precedence, port parsing) untested, and its interactive flows have no seams.

### Current Pattern

Live-only testing: the agent's decision logic is only exercised through `#[ignore]`d end-to-end Docker tests; nothing runs in CI.

### Target Pattern

- Extract pure decision functions where they're entangled with I/O (most already are pure after the batch_run module split).
- Unit-test them against fixtures: proxy reuse matrix (granted/not-granted × host-observers × partial/full), host-network gate, target-set expansion, resource override precedence (compose < dashboard override < global default).
- CLI: unit tests for `public_service_candidates`, `pick/auto_pick_public_service` (via YAML fixtures), `env_precedence_score`/`discover_env_files` ordering, `findings` grouping.
- Add `cargo-llvm-cov` as a documented local tool (not a CI gate initially) so gaps are visible: `cargo llvm-cov --workspace --ignore-filename-regex 'tests\.rs|main\.rs'`.

### Scope

| Area | Files | Tests to add (est.) |
|---|---|---|
| Agent plan decisions | `agent/src/routes/containers/batch_run/prepare.rs` | 8–12 unit tests |
| Agent rollback helpers | `batch_run/rollback.rs`, `types.rs` | 2–3 |
| Agent metadata/scan handlers | `metadata.rs`, `scan.rs` | 3–4 |
| CLI ship pure logic | `cli/src/ship/public_service.rs`, `env.rs`, `ui.rs` | 8–10 |
| CLI build context guard | `cli/src/build/context.rs` (uses tempdirs) | 2–3 |

### Considerations

- The `prepare.rs` phases take `&AgentState` — decide between thin trait seams (trait DockerOps) or refactoring the pure parts to take plan values only. Prefer the latter; avoid mocking Docker.
- Live tests stay as-is (`--ignored`, local-only, documented in development.md); they remain the end-to-end check.
- Coverage number is directional only — don't gate CI on a %.

### Priority

Medium-high — the agent is the least-tested crate despite owning container lifecycle; the batch_run split made this cheap to fix.

---
