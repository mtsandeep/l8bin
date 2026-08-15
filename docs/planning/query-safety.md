# SQL Query Safety

## Context

336 `sqlx::query` calls across the orchestrator, all with inline SQL strings. Two classes of silent failure are possible today:

1. **Typo'd SQL or column names** — caught only at runtime, per call site.
2. **Hand-written status literals** — 37 SQL statements embed strings like `'running'`, `'deploying'`, `'stopping'` in WHERE/CASE clauses while a `ProjectStatus` enum exists. A typo (`'runing'`) compiles, matches nothing, and fails silently.

## Current Pattern

```rust
// literal duplicated between SQL and the enum's Display
"SELECT ... WHERE status = 'running' AND auto_stop_enabled = 1"
sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM projects WHERE id = ?")
```

## Target Pattern

**Step 1 (cheap, do first):** every status literal in SQL goes through the enum — either bind `ProjectStatus::Running.to_string()` as a parameter instead of inlining, or `impl ProjectStatus { pub const fn as_str(&self) -> &'static str }` and use named constants in SQL. Zero behavior change; grep-auditable (`grep "'running'"` must return nothing).

**Step 2 (optional, larger):** adopt sqlx compile-time checked macros (`query!` / `query_as!`) for the orchestrator. Requires a `DATABASE_URL` (or offline `sqlx prepare` cache committed for CI) pointed at a migrated SQLite file. Catches column/typo/type errors at build time across all 336 call sites.

## Scope

| Piece | Notes |
|---|---|
| Status-literal purge | ~37 statements; mechanical |
| `ProjectStatus::as_str` | litebin-common `types.rs`; replaces ad-hoc `to_string()` in SQL binds too |
| sqlx offline cache (step 2) | `.sqlx/` directory committed; CI already has `--locked`-style discipline to extend |

## Considerations

- Step 2 changes every call site's macro name; do it per-module over time, not big-bang.
- Some queries build dynamic SQL (QueryBuilder in `routes/heartbeat.rs`, stats) — those stay runtime-checked by nature.
- SQLite schema lives in embedded migrations (`sqlx::migrate!`), so the prepare cache must be regenerated when migrations change — add a CI check that `cargo sqlx prepare --check` passes.

## Priority

Low-medium — step 1 is cheap and worth doing opportunistically; step 2 only when touching SQL-heavy code anyway.
