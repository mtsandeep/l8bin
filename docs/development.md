# Development Guide

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

### 2. Build & Stage for Installer Testing

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

The `litebin/docker-compose.yml` is a simplified setup for local development. It exposes the orchestrator port directly (no Caddy proxy needed).

```bash
cd litebin
docker compose up -d --build
```

This gives you:
- Orchestrator on `http://localhost:5080`
- Dashboard via Caddy on `http://l8bin.localhost`

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

```bash
bash setup/generate-certs.sh
```

Creates:
- Root CA (`certs/root-ca.pem`)
- Master server cert + key (`certs/master.{pem,key}`)
- Per-node client certs (`certs/nodes/<node-id>/{cert,key}.pem`)

See [Multi-Server Setup](multi-server.md) for full details.

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

See [Configuration](configuration.md) for the full reference.

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

When adding a new orchestrator API endpoint, update the Caddy path matcher so the request reaches the orchestrator instead of falling through to the dashboard:

1. Add the path to `ORCHESTRATOR_API_PATHS` in `litebin-common/src/caddy.rs` — this is the single source of truth used by all Caddy config builders (master_proxy, cloudflare_dns, and CaddyRouter).

2. No other files need updating — `routing.rs`, `cloudflare_router.rs`, and `caddy.rs` all reference the same constant.

3. The install scripts (`install.sh`, `install-windows.ps1`) have their own Caddyfile templates as a static fallback (used only before the orchestrator first pushes dynamic config). Update those too for consistency.

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

## Further Reading

- [Architecture](architecture.md) — system overview and component responsibilities
- [Configuration](configuration.md) — all environment variables
- [Release Process](release.md) — versioning and publishing releases
- [Local Testing](local-testing.md) — testing the install/update flow locally
- [API Reference](api-reference.md) — orchestrator API endpoints
- [CLI Reference](cli.md) — `l8b` command documentation
