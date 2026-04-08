# Release Process

LiteBin uses [cargo-release](https://github.com/crate-ci/cargo-release) to keep all crates in sync with a single version number.

## Setup

```bash
cargo install cargo-release
```

## How it works

All crates share a single version defined in the workspace root (`Cargo.toml`):

```toml
[workspace.package]
version = "0.1.0"
```

Each member inherits it:

```toml
[package]
version = { workspace = true }
```

When you run `cargo-release`, it:

1. Bumps the version in **all** `Cargo.toml` files
2. Updates `Cargo.lock`
3. Commits: `chore: release v<version>`
4. Creates git tag `v<version>`
5. Pushes the commit and tag to remote
6. The `release.yml` GitHub Action picks up the tag and builds all artifacts

**You do not need to manually tag commits.** `cargo-release` handles everything.

## Making a release

### Patch release (bug fix) — `0.1.0` -> `0.1.1`

```bash
cargo release patch
```

### Minor release (new feature) — `0.1.0` -> `0.2.0`

```bash
cargo release minor
```

### Major release (breaking change) — `0.1.0` -> `1.0.0`

```bash
cargo release major
```

### Dry run (preview without changing anything)

```bash
cargo release patch --dry-run
```

This shows exactly what files would change without making any commits or tags.

## What gets built

The GitHub Action (`release.yml`) builds on tag push (`v*`):

| Artifact | Platforms |
|---|---|
| `l8b` (CLI) | linux x86_64, linux aarch64, macOS x86_64, macOS aarch64, Windows x86_64 |
| `litebin-orchestrator` | linux x86_64, linux aarch64 |
| `litebin-agent` | linux x86_64, linux aarch64 |
| Dashboard (tar.gz + zip) | Static assets |

## Version in running services

- **CLI**: `l8b --version`
- **Orchestrator**: `GET /health` returns `{ version: "0.1.1" }`
- **Agent**: `GET /health` returns `{ version: "0.1.1" }` in the `HealthReport`

All use `env!("CARGO_PKG_VERSION")` which is set at compile time from the workspace version.

## Workflow summary

```
1. Make your changes on main
2. Ensure everything builds: cargo build --workspace
3. Run tests: cargo test --workspace
4. Release: cargo release patch  (or minor/major)
5. CI builds and publishes to GitHub Releases automatically
```
