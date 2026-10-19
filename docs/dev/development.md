# Development Guide

## Database migrations

Migrations are checksummed: once a migration file ships (or runs anywhere outside your machine), never edit it — sqlx refuses to start with `migration N was previously applied but has been modified`. Add a new migration instead. If a local dev database hits this error after editing an unshipped migration, update the checksum in `_sqlx_migrations` or delete the dev database.


## Prerequisites

- **Rust** 1.85+ (edition 2024)
- **Docker Desktop** (for running containers locally)
- **Node.js** + **pnpm** (only if working on the dashboard)
- **Git**

## Project Structure

```
litebin/
├── compose-bollard/      # Docker Compose YAML parser, validator, variable interpolation
├── litebin-common/       # Shared library (Docker, types, DB models, routing)
├── orchestrator/         # Master server binary (axum, SQLite, Caddy management)
├── agent/                # Worker node binary (axum, Docker, mTLS)
├── cli/                  # CLI tool (l8b) — build, deploy, ship, login
├── dashboard/            # React dashboard (pnpm, TypeScript)
├── landing/              # Landing page (static)
├── docs/                 # Documentation
├── setup/                # Build scripts and cert generation
├── test-apps/            # Sample apps for testing
├── install.sh            # Linux/macOS installer
├── install-windows.ps1   # Windows installer
├── docker-compose.yml    # Production compose (with profiles)
└── Caddyfile             # Caddy reverse proxy config
```

All Rust crates share a workspace defined in the root `Cargo.toml`.

---

## Quick Start

### 1. Check / Build

```bash
# Check all crates compile (fast, no binary output)
cargo check

# Build all crates in release mode
cargo build --release

# Build a specific crate
cargo build --release -p litebin-orchestrator
cargo build --release -p litebin-agent
cargo build --release -p l8b
```

### 2. Lint (must pass in CI)

CI runs these on every push/PR — run them locally before pushing:

```bash
# Formatting (rustfmt config is in rustfmt.toml; zero drift allowed)
cargo fmt --all -- --check

# Clippy with warnings denied — the workspace is warning-free, keep it that way
cargo clippy --workspace --all-targets -- -D warnings
```

Fix formatting with `cargo fmt --all`. If clippy reports a new warning, fix it
or, for intentionally verbose API signatures, add a targeted
`#[allow(clippy::too_many_arguments)]` on the function.

**Platform-gated code caveat:** clippy only lints code compiled for the host
platform — `#[cfg(unix)]` functions (e.g. `chown_bind_mounts`) are invisible to
clippy on Windows and only get linted by the Linux CI runner. When editing
unix-gated code on Windows, run clippy in a Linux container instead (uses the
toolchain pinned by `rust-toolchain.toml`; the named volume caches the dependency
build between runs):

```bash
docker run --rm -v "$(pwd):/workspace" -v l8bin-lint-cache:/tmp/lint-target \
  -w /workspace -e CARGO_TARGET_DIR=/tmp/lint-target \
  rust:1.94.1 cargo clippy --workspace --all-targets --locked -- -D warnings
```

(The image tag must match the `rust-toolchain.toml` pin. On git-bash, prefix the
command with `MSYS_NO_PATHCONV=1` if path translation mangles the mounts.)

### 3. Tests

```bash
# Fast suite (what CI runs) — unit tests + in-memory-SQLite integration tests
cargo test --workspace

# Tests for one crate
cargo test -p litebin-orchestrator

# Live agent tests — require a local Docker daemon with registry access.
# These are #[ignore]d and never run in CI; run them on demand:
cargo test -p litebin-agent -- --ignored
```

Test organization: unit tests live inline (`#[cfg(test)] mod tests` at the
bottom of the file or a sibling `tests.rs`), and the orchestrator's HTTP
integration suite lives in `orchestrator/src/tests/` using in-memory SQLite
with real migrations. Tests that need a real Docker daemon are `#[ignore]`d
and documented with their requirements.

### 4. Build & Stage for Installer Testing

These scripts build everything and copy binaries into a `release/` folder with the naming convention the installer expects.

**Linux / macOS:**
```bash
bash setup/build.sh
```

Output in `release/`:
```
l8b-x86_64-linux
litebin-agent-x86_64-linux
litebin-orchestrator-x86_64-linux
```

**Windows (PowerShell):**
```powershell
powershell -ExecutionPolicy ByPass -File setup\build.ps1
```

Output in `release\`:
```
l8b-x86_64-windows.exe
litebin-agent-x86_64-windows.exe
litebin-orchestrator-x86_64-windows.exe
```

### 3. Test the Installer Locally

**Linux / macOS** (after running `setup/build.sh`):
```bash
# Install CLI from local release
curl -fsSL https://l8b.in | L8B_RELEASE_DIR=./release bash -s cli

# Or test master install
L8B_RELEASE_DIR=./release bash install.sh
```

**Windows** (after running `setup\build.ps1`):
```powershell
# Install CLI from local release
$env:L8B_RELEASE_DIR = ".\release"
powershell -ExecutionPolicy ByPass -File .\install-windows.ps1 cli

# Or test master install
powershell -ExecutionPolicy ByPass -File .\install-windows.ps1
```

### 4. Test the Update Flow (Linux/macOS)

Builds orchestrator + dashboard and stages into `local-release/`:
```bash
bash setup/prepare-local-release.sh

# Then test the update:
L8B_RELEASE_DIR=./local-release bash install.sh update
```

---

## Running with Docker

### Production Compose (Linux)

The root `docker-compose.yml` uses Docker profiles to control which services run.

```bash
# Master server (orchestrator + dashboard + caddy)
docker compose --profile master up -d --build

# Master + local agent (for multi-node testing)
docker compose --profile master --profile agent up -d --build
```

**Profiles:**

| Profile | Services | Use case |
|---|---|---|
| `master` | orchestrator, dashboard, caddy | Single-node production setup |
| `agent` | agent, agent-caddy | Local agent for multi-node testing |
| (none) | caddy only | Caddy standalone |

Copy `.env.example` to `.env` and configure before running:
```bash
cp .env.example .env
```

### Local Dev Compose (Windows / any platform)

The `litebin/docker-compose.yml` fronts everything with Caddy; the orchestrator port is not published to the host.

```bash
cd litebin
docker compose up -d --build
```

This gives you:
- Dashboard & API via Caddy on `http://l8bin.localhost` — browsers, curl, and the `l8b` CLI all resolve `*.localhost` to loopback themselves (RFC 6761; OS resolvers vary — Windows' doesn't — so other tools may need a `127.0.0.1 l8bin.localhost` hosts entry, which the Windows installer adds)
- `l8b login --server http://l8bin.localhost`

`http://localhost:5080` only works when the orchestrator is directly reachable: the Windows installer's local-mode compose publishes it, and the native `cargo run` flow below listens on it.

---

## Running Binaries Directly (No Docker)

For development, you can run the orchestrator or agent directly without Docker containers.

### Orchestrator

```bash
# Build
cargo build -p litebin-orchestrator

# Run (needs Docker running for container management)
./target/debug/litebin-orchestrator
```

The orchestrator reads config from environment variables or a `.env` file in the working directory. At minimum:
```
DOMAIN=localhost
PORT=5080
DATABASE_URL=sqlite:./data/litebin.db
```

### Agent

```bash
# Build
cargo build -p litebin-agent

# Run (needs Docker + mTLS certs from orchestrator)
./target/debug/litebin-agent
```

### CLI

```bash
# Build
cargo build -p l8b

# Run
./target/debug/l8b --help
./target/debug/l8b ship
# Against the native orchestrator (above); with the compose stack instead: http://l8bin.localhost
./target/debug/l8b login --server http://localhost:5080
```

---

## Cross-Compilation

The `.cargo/config.toml` configures the linker for `aarch64-unknown-linux-gnu` (ARM64 Linux).

```bash
# Build for ARM64 Linux (e.g., Raspberry Pi, ARM servers)
cargo build --release --target aarch64-unknown-linux-gnu
```

---

## Dashboard Development

```bash
cd dashboard

# Install dependencies
pnpm install

# Dev server
pnpm dev

# Production build
pnpm build
```

---

## Generating mTLS Certificates (Multi-Node)

Production (shared agent cert bundle):

```bash
curl -fsSL https://l8b.in | bash -s certs
```

Creates on the master: Root CA, master `server.pem` / `server-key.pem`, and one shared `agent.pem` / `agent-key.pem` distributed to every agent.

For local/dev experiments, `bash setup/generate-certs.sh` can still mint certs under `certs/`; prefer the installer path above for real multi-server setups. See [Multi-Server Setup](../topics/multi-server.md).

---

## Environment Variables

Copy `.env.example` to `.env` for a full list. Key variables:

| Variable | Default | Description |
|---|---|---|
| `DOMAIN` | `localhost` | Server domain |
| `DASHBOARD_SUBDOMAIN` | `l8bin` | Dashboard URL: `{sub}.{domain}` |
| `PORT` | `5080` | Orchestrator HTTP port |
| `DATABASE_URL` | `sqlite:./data/litebin.db` | SQLite database path |
| `DOCKER_NETWORK` | `litebin-network` | Docker network name |
| `ROUTING_MODE` | `master_proxy` | `master_proxy` or `cloudflare_dns` |
| `DEFAULT_AUTO_STOP_MINS` | `15` | Idle auto-stop timeout |
| `JANITOR_INTERVAL_SECS` | `300` | Janitor cleanup interval |

See [Configuration](../topics/configuration.md) for the full reference.

---

## Workspace Crates

| Crate | Binary | Description |
|---|---|---|
| `compose-bollard` | — | Docker Compose YAML parser, validator, and variable interpolation |
| `litebin-common` | — | Shared types, Docker client, routing, compose run planning |
| `orchestrator` | `litebin-orchestrator` | Master server: API, Caddy, DB, auth, deploy |
| `agent` | `litebin-agent` | Worker node: container lifecycle, local Caddy, mTLS |
| `cli` | `l8b` | CLI: build, ship, deploy, login, project management |

---

## Adding a New API Endpoint

Add the axum route in `orchestrator/src/app.rs` and its handler — that's it. The orchestrator is the sole router for the dashboard host: Caddy sends it everything, registered routes serve the API, and the `waker::wake` fallback streams anything else to the dashboard SPA (`DASHBOARD_UPSTREAM`).

---

## Common Tasks

```bash
# Check for compilation errors (fast)
cargo check

# Run clippy lints
cargo clippy -- -D warnings

# Run tests
cargo test

# Build all in release mode
cargo build --release

# Build and stage for installer (Linux/macOS)
bash setup/build.sh

# Build and stage for installer (Windows)
powershell -ExecutionPolicy ByPass -File setup\build.ps1

# Build orchestrator + dashboard for local update testing
bash setup/prepare-local-release.sh

# Start full stack with Docker (Linux)
docker compose --profile master up -d --build

# Start simplified stack (Windows/local dev)
cd litebin && docker compose up -d --build

# Generate mTLS certs
bash setup/generate-certs.sh
```

---

## Agent API (Typed Wire Contract)

The orchestrator ↔ agent internal protocol is defined once, in code, and shared by both sides: every request/response body is a struct in [`litebin-common/src/agent_api.rs`](../../litebin-common/src/agent_api.rs), and the orchestrator sends/receives them through the typed [`AgentClient`](../../orchestrator/src/nodes/client.rs). Renaming a field breaks compilation on the other side instead of failing at runtime in production.

| Piece | Where | Role |
|---|---|---|
| DTOs + endpoint path constants | `litebin-common/src/agent_api.rs` | One struct per internal endpoint, used by BOTH crates; wire-format tests pin the serialized field names |
| `AgentClient` + `AgentClientError` | `orchestrator/src/nodes/client.rs` | One typed method per endpoint; centralizes URL building and the 503/502/500 error convention |
| `build_batch_run_payload` | `orchestrator/src/routes/manage/multi_service/helpers.rs` | Single implementation of the batch-run payload (service resource overrides + global defaults read once), shared by start/recreate/deploy/stage |
| Agent handlers | `agent/src/routes/*` | Extract/respond with the shared DTOs directly (local struct definitions re-export from `agent_api`) |

`AgentClient::resolve(state, node_id)` performs the DB lookup + client pool lookup + base-URL derivation that was previously repeated at every call site.

**Error convention:** `Transport` → 503 (agent unreachable), `Status { code, body }` → 502 (agent answered non-success), `Parse` → 500 (response didn't match the DTO). Call sites with special semantics match variants directly (e.g. reconciliation treats `Status` with 404 as "container gone"; the waker falls back to recreate when `/containers/start` fails).

**Compatibility rules:**
- Field names and serde attributes in `agent_api` **are** the wire format — don't change them without a `protocol_version` bump (see `types::HealthReport`).
- Requests: `Option` fields serialize as `null` when `None`; the agent's deserialization treats absent and `null` identically.
- Responses: fields with `skip_serializing_if` also carry `serde(default)` so clients tolerate their absence.
- The unit tests at the bottom of `agent_api.rs` pin the serialized shapes for the high-traffic endpoints; keep them passing.
- `/caddy/sync` request bodies stay raw `serde_json::Value` — the Caddy config is dynamic JSON by design; only the transport is typed.

---

## Further Reading

- [Architecture](../architecture/architecture.md) — system overview and component responsibilities
- [Configuration](../topics/configuration.md) — all environment variables
- [Release Process](release.md) — versioning and publishing releases
- [API Reference](../reference/api-reference.md) — orchestrator API endpoints
- [CLI Guide](../reference/cli.md) — `l8b` command documentation
