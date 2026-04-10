# Release Process

LiteBin uses [cargo-release](https://github.com/crate-ci/cargo-release) to keep all crates in sync with a single version number and auto-update the changelog.

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

1. Updates the changelog (moves `[Unreleased]` entries to the new version)
2. Bumps the version in **all** `Cargo.toml` files
3. Updates `Cargo.lock`
4. Commits: `chore: release v<version>`
5. Creates git tag `v<version>`
6. Pushes the commit and tag to remote
7. The `release.yml` GitHub Action picks up the tag and builds all artifacts

**You do not need to manually tag commits or edit the changelog header.** `cargo-release` handles everything.

## Changelog

We maintain a single `CHANGELOG.md` at the workspace root (not per-crate) since LiteBin ships as one unit.

### Format

```markdown
## [Unreleased]

### Added
- New feature description

### Changed
- Change description

### Fixed
- Bug fix description

## [0.1.5] - 2026-04-10

### Added
- ...

### Changed
- ...
```

### How to update

During development, add entries under `## [Unreleased]` in the appropriate section (`Added`, `Changed`, or `Fixed`). That's it — don't create version headers or dates manually. `cargo-release` handles that on release.

### What happens on release

When you run `cargo-release`, it finds `## [Unreleased]` and transforms it into:

```markdown
## [Unreleased]

## [0.1.6] - 2026-04-10

### Added
- (your entries move here)
...
```

The old unreleased content becomes the new version entry, and a fresh empty `[Unreleased]` is created for the next cycle.

### Why the config is in cli/Cargo.toml

`cargo-release` applies workspace-level config to every crate. If changelog replacements were in `release.toml`, it would try to find `CHANGELOG.md` in each crate's directory (`litebin-common/`, `orchestrator/`, etc.) and fail. Instead, the replacement config lives only in `cli/Cargo.toml` (as `[package.metadata.release]`), so only the `l8b` package runs it, pointing at `../CHANGELOG.md`. This is a workaround for the fact that `cargo-release` is designed for per-crate changelogs, while we use a single repo-level one.

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
2. Add changelog entries under ## [Unreleased] in CHANGELOG.md
3. Ensure everything builds: cargo build --workspace
4. Run tests: cargo test --workspace
5. Release: cargo release patch  (or minor/major)
6. CI builds and publishes to GitHub Releases automatically
```
