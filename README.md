# LiteBin | L8Bin

> **Built for side projects, not SLAs.** Your demos, previews, and portfolio apps — actually running, on your own domain, on a VPS you already pay for.
>
> → [l8bin.com](https://l8bin.com) for the full picture.

Self-hosted App Manager. Deploy single or multi-service apps from your dashboard, CLI, or GitHub Actions — with volume persistence, custom routing, and automatic HTTPS.

## Why LiteBin

Cloud hosting platforms are great until you check the bill for side projects you built months ago. Self-hosted PaaS platforms are built for production workloads — customer-facing apps that need uptime guarantees, multi-tenant isolation, and rollback safety. Your weekend demo doesn't need that overhead.

LiteBin runs on **one cheap VPS** ($5-10/mo) and sleeps apps when nobody's looking. No per-app fees, no credit card on file, no vendor lock-in. Deploy from your laptop or GitHub Actions — your Docker image runs as-is, behind your own domain, with automatic HTTPS.

It's built for engineers who want their side projects, demos, and portfolio apps **actually running** — not just committed to a repo or sitting in a local Docker container.

## Design Decisions

| Choice | Why |
|---|---|
| **Rust** (orchestrator, agent, CLI) | Single binary, low memory, fast cold starts, reliable long-running services |
| **Caddy** (reverse proxy) | Dynamic config via admin API, automatic HTTPS, JSON config pushed programmatically |
| **SQLite** (WAL mode) | Zero ops, single-file, fast enough for this scale — no Postgres to manage |
| **Docker** (runtime) | Standard container format, works with any registry, familiar tooling |
| **Docker Compose** (multi-service) | Deploy multi-container apps from `docker-compose.yml` — per-project networks, dependency-ordered startup, selective service rebuilds |
| **No Kubernetes** | LiteBin manages 1-50 apps on 1-5 servers. K8s adds complexity with no benefit at this scale |
| **Scale-to-zero for web apps** | Idle web projects can sleep and wake on HTTP requests, saving resources without losing on-demand availability |

More rationale in [Design Decisions](docs/architecture/decisions.md).

## Routing Modes

LiteBin has two routing modes, switchable from the dashboard at any time:

**Master Proxy** (default) — All traffic flows through the master server. Simple setup: one wildcard DNS record. Trade-off: master sees 2x bandwidth (proxies both directions).

**Cloudflare DNS** — DNS points directly to agent nodes. Each agent handles its own traffic independently. Trade-off: requires a Cloudflare account. The key advantage: **agents continue serving traffic even if the master is down.**

See [Architecture](docs/architecture/architecture.md) for the full breakdown, and [Configuration → DNS Records](docs/topics/configuration.md) for the records each mode needs.

## Quick Start

```bash
# Interactive — asks what to install
curl -fsSL https://l8b.in | bash

# Master server (orchestrator + dashboard + Caddy)
curl -fsSL https://l8b.in | bash -s master

# Agent
curl -fsSL https://l8b.in | bash -s agent

# CLI only
curl -fsSL https://l8b.in | bash -s cli
```

Run the master on a Linux VPS as root — the installer sets up Docker if it's missing. The guided walkthrough (prompts, DNS, admin account, first deploy) lives at **[l8bin.com/quickstart](https://l8bin.com/quickstart)**.

## Deploying Apps

- **CLI** — `l8b ship` builds on your machine and streams the image to the server. No registry needed; Dockerfile optional (Railpack auto-detects). ([CLI reference](docs/reference/cli.md))
- **Dashboard** — click Deploy and enter any public-registry image, or upload a `docker-compose.yml` for multi-service apps.
- **GitHub** — the dashboard's GitHub App writes the workflow + secrets into your repo ([github-connect](docs/topics/github-connect.md)), or wire [l8bin-action](https://github.com/mtsandeep/l8bin-action) by hand.
- **AI agents** — give Cursor or Claude the LiteBin MCP server and let them deploy, verify, and fix end-to-end: [l8bin.com/agents](https://l8bin.com/agents).

Idle apps sleep to zero and wake on the first request; any project can be pinned always-on.

## Architecture

```
┌──────────────┐     ┌───────────────┐     ┌────────────────┐
│  Dashboard   │     │  Orchestrator │     │  Agent (opt.)  │
│  (React)     │     │  (Rust)       │     │  (Rust)        │
│  nginx:alpine│     │  API + Docker │     │  Docker mTLS   │
└──────┬───────┘     └──────┬────────┘     └───────┬────────┘
       │                    │                      │
       └────────────────────┼──────────────────────┘
                            │
                     ┌──────┴────────┐
                     │    Caddy      │
                     │  Reverse Proxy│
                     │  Auto HTTPS   │
                     └───────────────┘
```

- **Orchestrator** — API server, container lifecycle, route management, compose parsing, volume scoping, auth, heartbeat, janitor (Rust, axum)
- **Dashboard** — React UI for deploying apps, managing nodes, configuring settings, and managing services and volumes
- **Caddy** — reverse proxy with automatic TLS termination, dynamic routing via admin API, and custom route proxy support (path-based and subdomain alias)
- **Agent** — runs on remote servers, manages containers locally, handles wake requests autonomously via mTLS

See [Architecture](docs/architecture/architecture.md) for the detailed component breakdown, routing modes, and links to all technical docs.

## Multi-Node (Optional)

Mix any servers, any providers — each agent hosts its own set of apps:

```bash
# On the master — generates mTLS certs, prints a cert bundle
curl -fsSL https://l8b.in | bash -s certs

# On each agent — prompts for the cert bundle
curl -fsSL https://l8b.in | bash -s agent
```

Or start from the dashboard — **Nodes → Add Node** — which walks you through it. Details in [Multi-Server Setup](docs/topics/multi-server.md).

## Documentation

| Topic | Link |
|---|---|
| Architecture | [docs/architecture/architecture.md](docs/architecture/architecture.md) |
| Design decisions | [docs/architecture/decisions.md](docs/architecture/decisions.md) |
| Multi-server (agents, mTLS) | [docs/topics/multi-server.md](docs/topics/multi-server.md) |
| Volumes & persistence | [docs/topics/volumes.md](docs/topics/volumes.md) |
| Configuration & DNS records | [docs/topics/configuration.md](docs/topics/configuration.md) · [`.env.example`](.env.example) |
| GitHub connect | [docs/topics/github-connect.md](docs/topics/github-connect.md) |
| Security · Backup | [docs/topics/security.md](docs/topics/security.md) · [docs/topics/backup.md](docs/topics/backup.md) |
| FAQ / troubleshooting | [docs/topics/faq.md](docs/topics/faq.md) |
| CLI reference · REST API | [docs/reference/cli.md](docs/reference/cli.md) · [API docs](https://l8bin.com/docs) · [openapi.json](https://l8bin.com/docs/openapi.json) |
| llms.txt (for agents) | [l8bin.com/llms.txt](https://l8bin.com/llms.txt) · [l8bin.com/llms-full.txt](https://l8bin.com/llms-full.txt) (with API spec) |

## Development

Requires Rust, Docker, and optionally Node.js + pnpm (for the dashboard).

```bash
# Build all
cargo build --release

# Check compilation (fast, no binary)
cargo check

# Start full stack
docker compose --profile master up -d --build
```

On Windows with Docker Desktop, the installer script runs the full stack locally:

```powershell
iex (irm https://l8b.in/windows.ps1)   # add -Clean to tear it down
```

Caddy generates a self-signed cert for `*.localhost` — expect a browser warning.

See the [Development Guide](docs/dev/development.md) for build scripts, local testing, Windows development, and cross-compilation.
