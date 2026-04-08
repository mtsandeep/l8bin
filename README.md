# LiteBin | L8Bin

Self-hosted PaaS. Deploy apps from your dashboard, CLI, or GitHub Actions.

## Quick Start

```bash
# Master server (orchestrator + dashboard + Caddy)
curl -sSL https://l8b.in | bash-s master

# Worker node
curl -sSL https://l8b.in | bash-s agent

# CLI only
curl -sSL https://l8b.in | bash-s cli
```

---

## Master Server Setup

Run on a Linux VPS with Docker installed. Requires a domain with DNS pointed to the server.

```bash
curl -sSL https://l8b.in | bash-s master
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
2. Deploy apps using any method below

### Multi-node (optional)

To add worker nodes, run the multi-node setup on the master server:

```bash
# On the master — generates mTLS certs, prints a cert bundle
curl -sSL https://l8b.in | bash -s certs
```

This generates ECDSA P-256 mTLS certificates, configures the master, and prints a compact cert bundle.

#### To regenerate certificates

Re-run the same command. This **invalidates all existing agent connections** — update each agent afterward:

```bash
# On each worker node — prompts for new cert bundle
curl -sSL https://l8b.in | bash -s agent --update-certs
```

---

## Worker Node Setup

Run on a separate Linux server. Requires Docker.

You can also start this from the **dashboard** -> **Nodes** -> **Add Node**, which shows the install command to copy.

```bash
curl -sSL https://l8b.in | bash -s agent
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
curl -sSL https://l8b.in | bash-s cli
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

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: l8bin/l8bin-action@v1
        with:
          server: ${{ secrets.L8B_SERVER }}
          token: ${{ secrets.L8B_TOKEN }}
          project_id: myapp
          port: 3000
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
| Dashboard | `https://l8bin.localhost` |
| API | `http://localhost:5080` |

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

- **Orchestrator** — manages app containers, handles deploy API, syncs routes to Caddy
- **Dashboard** — React UI for managing projects, nodes, and settings
- **Caddy** — reverse proxy with automatic TLS, dynamic routing via admin API
- **Agent** — runs on worker nodes, manages containers remotely via mTLS

---

## Configuration

All config is in `.env` after install. Key variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `DOMAIN` | Your server domain | *(required on Linux)* |
| `DASHBOARD_SUBDOMAIN` | Dashboard subdomain | `l8bin` |
| `ROUTING_MODE` | `master_proxy` or `cloudflare_dns` | `master_proxy` |
| `IDLE_TIMEOUT_SECS` | Seconds before idle apps sleep | `900` (15 min) |
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
