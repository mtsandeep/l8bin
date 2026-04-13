# LiteBin | L8Bin

> **Built for side projects, not SLAs.** Your demos, previews, and portfolio apps — actually running, on your own domain, on a VPS you already pay for.
>
> → [l8bin.com](https://l8bin.com) for the full picture.

Self-hosted App Manager. Deploy apps from your dashboard, CLI, or GitHub Actions.

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
| **No Kubernetes** | LiteBin manages 1-50 apps on 1-5 servers. K8s adds complexity with no benefit at this scale |
| **Scale-to-zero by default** | Side projects sit idle 99% of the time. Sleeping containers saves resources without losing the ability to serve traffic on demand |

## Routing Modes

LiteBin has two routing modes, switchable from the dashboard at any time:

**Master Proxy** (default) — All traffic flows through the master server. Simple setup: one wildcard DNS record. Trade-off: master sees 2x bandwidth (proxies both directions).

**Cloudflare DNS** — DNS points directly to agent nodes. Each agent handles its own traffic independently. Trade-off: requires a Cloudflare account. The key advantage: **agents continue serving traffic even if the master is down.**

See [Architecture](docs/architecture.md) for the full breakdown.

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

---

## Master Server Setup

Run on a Linux VPS with Docker installed. Requires a domain with DNS pointed to the server.

```bash
curl -fsSL https://l8b.in | bash -s master
```

The installer will prompt for:

| Prompt | Description | Default |
|--------|-------------|---------|
| Domain | Your server's domain (e.g. `example.com`) | *(required)* |
| Dashboard subdomain | Subdomain for the dashboard | `l8bin` |
| Poke subdomain | Subdomain for agent wake endpoint | `poke` |
| Routing mode | `master_proxy` or `cloudflare_dns` | `master_proxy` |

After setup:

1. Open `https://l8bin.example.com` and create an admin account
2. Configure DNS for your domain (see below)
3. Deploy apps using any method below

### DNS Setup

Create **DNS-only** (grey cloud, not proxied) A records in your DNS provider. The records depend on your routing mode:

| Routing Mode | Records to Create | Managed By |
|---|---|---|
| `master_proxy` (default) | `*.{domain}` → master IP | Manual (one wildcard) |
| `cloudflare_dns` | `{DASHBOARD_SUBDOMAIN}.{domain}` → master IP<br>`{POKE_SUBDOMAIN}.{domain}` → master IP | Manual (2 records) |
| | All app subdomains (e.g. `{project}.{domain}`) | **Automatic** via Cloudflare API |

> **cloudflare_dns mode:** Do NOT create a wildcard (`*`) record. It is not needed and will conflict with the per-project records created automatically. Also requires `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ZONE_ID` in your `.env` or Dashboard Settings.

### Multi-node (optional)

To add agents, run the multi-node setup on the master server:

```bash
# On the master — generates mTLS certs, prints a cert bundle
curl -fsSL https://l8b.in | bash -s certs
```

This generates ECDSA P-256 mTLS certificates, configures the master, and prints a compact cert bundle.

#### To regenerate certificates

Re-run the same command. This **invalidates all existing agent connections** — update each agent afterward:

```bash
# On each agent — prompts for new cert bundle
curl -fsSL https://l8b.in | bash -s agent --update-certs
```

---

## Agent Setup

Run on a separate Linux server. Requires Docker.

You can also start this from the **dashboard** -> **Nodes** -> **Add Node**, which shows the install command to copy.

```bash
curl -fsSL https://l8b.in | bash -s agent
```

The installer will prompt for:

| Prompt | Description |
|--------|-------------|
| Master dashboard URL | Your master's dashboard URL (e.g. `https://l8bin.example.com`) |
| Node name | A name for this worker (e.g. `worker-1`) |
| Agent port | Host port for the agent (default: `5083`) |
| Cert bundle | Paste the cert bundle from the multi-node setup |

After setup, go to the master dashboard -> **Nodes** -> **Add Node**, enter the node name and the worker's public IP, then click **Connect**.

---

## Deploying Apps

### Option 1: Dashboard

Open the dashboard and click **Deploy**. Enter an image from any public registry:

```
nginx:alpine
node:20
ghcr.io/org/app:latest
```

Dashboard deploys only support pre-built images from public registries. Private registry support is coming soon.

### Option 2: CLI

Install the CLI:

```bash
curl -fsSL https://l8b.in | bash -s cli
```

Log in to your server:

```bash
l8b login --server https://l8bin.example.com
```

Deploy from a local project (auto-detects Dockerfile or uses Railpack):

```bash
l8b ship
```

Or deploy non-interactively:

```bash
l8b deploy --project myapp --port 3000
```

For CI/CD, use a deploy token (created from the dashboard):

```bash
export L8B_TOKEN=your-token-here
l8b deploy --project myapp --port 3000
```

### Option 3: GitHub Actions

Add a workflow to your repo:

```yaml
name: Deploy
on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: mtsandeep/l8bin-action@v1
        with:
          server: ${{ secrets.L8B_SERVER }}
          token: ${{ secrets.L8B_TOKEN }}
          project_id: myapp
```

---

## Running Locally on Windows

For development and testing on Windows with Docker Desktop.

### Prerequisites

- Windows with Docker Desktop

### Quick start

```powershell
iex (irm https://l8b.in/windows.ps1)
```

Downloads release binaries from GitHub, generates Dockerfiles + docker-compose, and starts everything. Defaults to `localhost` (domain) and `l8bin.localhost` (dashboard).

Optional flags:

```powershell
iex (irm https://l8b.in/windows.ps1) -Domain example.com -DashboardSub l8bin
```

To clean up:

```powershell
iex (irm https://l8b.in/windows.ps1) -Clean
```

### What you get

| Component | URL |
|-----------|-----|
| Dashboard & API | `https://l8bin.localhost` |

Caddy generates a self-signed TLS certificate for `*.localhost`. Your browser will show a certificate warning — trust it or import the root cert from Docker.

---

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

- **Orchestrator** — API server, container lifecycle, route management, auth, heartbeat, janitor (Rust, axum)
- **Dashboard** — React UI for deploying apps, managing nodes, and configuring settings
- **Caddy** — reverse proxy with automatic TLS termination and dynamic routing via admin API
- **Agent** — runs on remote servers, manages containers locally, handles wake requests autonomously via mTLS

See [Architecture](docs/architecture.md) for detailed component breakdown, routing modes, and links to all technical docs. See [Troubleshooting FAQ](docs/faq.md) for common issues.

---

## Configuration

All config is in `.env` after install. Key variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `DOMAIN` | Your server domain | *(required on Linux)* |
| `DASHBOARD_SUBDOMAIN` | Dashboard subdomain | `l8bin` |
| `ROUTING_MODE` | `master_proxy` or `cloudflare_dns` | `master_proxy` |
| `DEFAULT_AUTO_STOP_MINS` | Mins before idle apps sleep | `900` (15 min) |
| `JANITOR_INTERVAL_SECS` | Janitor check interval | `300` (5 min) |

See [`.env.example`](.env.example) for the full list.

---

## Development

Requires Rust, Node.js 24, pnpm, and Docker.

```bash
# Build all Rust binaries
cargo build --release

# Build CLI only
cargo build --release -p l8b

# Build orchestrator only
cargo build --release -p litebin-orchestrator

# Build agent only
cargo build --release -p litebin-agent

# Build dashboard
cd dashboard && pnpm install && pnpm build

# Start dev stack (orchestrator + dashboard + caddy)
docker compose --profile master up -d

# Run orchestrator tests
cargo test -p litebin-orchestrator
```
