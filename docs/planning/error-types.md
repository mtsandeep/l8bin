# anyhow → thiserror for Typed Errors

## Context

All crate-internal errors use `anyhow::Error` with `bollard::Error` and `sqlx::Error` erased via `.into()`. Callers that need to distinguish error cases (e.g., 404 vs 500) must downcast or string-match. We've already fixed string-matching (P2/P4), but the underlying types are still erased.

Converting key error paths to `thiserror` enums gives callers typed match arms instead of downcasting.

## Current Pattern (to replace)

```rust
// DockerManager methods — all return anyhow
pub async fn stop_container(&self, id: &str) -> anyhow::Result<()> { ... }

// Callers classify errors via downcast
match DockerErrorKind::from_anyhow(&e) {
    DockerErrorKind::NotFound => ...,
    _ => ...,
}
```

## Target Pattern

```rust
#[derive(thiserror::Error, Debug)]
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

## Scope

| Error type | File(s) | Impact |
|---|---|---|
| `DockerError` | `litebin-common/src/docker/` | All `DockerManager` methods |
| `DbError` | `orchestrator/src/` | SQL queries + UNIQUE constraint detection |
| `CloudflareError` | `litebin-common/src/cloudflare.rs` | DNS API calls |

## Considerations

- `DockerManager` methods are called from axum handlers that return `impl IntoResponse` — these already use `match` for error mapping, so the change is mostly mechanical.
- `DbError` would replace `is_unique_constraint()` helper in `validation.rs` with `DbError::UniqueConstraint` variant.
- `CloudflareError` would replace the `code == Some(81057)` check with `CloudflareError::DuplicateRecord`.
- The `orchestrator` → `agent` HTTP calls use `reqwest` — these get a `ClientError` type via `docs/planning/agent-wire-contract.md`, which depends on this effort.
- Callers outside the workspace (none currently) would need the error types re-exported from `litebin-common`.
- Scope note (2026-08 maintainability review): beyond the internal errors above, 41 orchestrator handler signatures return `(StatusCode, String)` — the same effort should introduce one `ApiError` enum with a central `IntoResponse` impl so error response shapes are consistent for the dashboard.

## Priority

Medium — still code quality / ergonomics only (no behavioral changes), but it is a prerequisite for `agent-wire-contract.md` and unlocks consistent dashboard error handling across the 41 `(StatusCode, String)` handler signatures.
