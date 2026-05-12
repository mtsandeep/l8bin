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
- The `orchestrator` → `agent` HTTP calls use `reqwest` — these could get a `ClientError` type.
- Callers outside the workspace (none currently) would need the error types re-exported from `litebin-common`.

## Priority

Low — This is code quality / ergonomics only. No behavioral changes.
