# Architecture

## Overview

LiteBin is a self-hosted PaaS that runs on a single VPS or scales across multiple nodes. It deploys apps as Docker containers, sleeps them when idle, and wakes them on request.

```
Browser -> Caddy (reverse proxy, auto-TLS) -> Orchestrator (Rust)
                                                    |
                                              +----+----+
                                              |         |
                                         Dashboard  Agent(s)
                                         (React)    (Rust)
```

## Components

| Component | Technology | Role |
|---|---|---|
| **Orchestrator** | Rust (axum, bollard, sqlx) | API server, container management, routing, auth |
| **Dashboard** | React | Web UI for managing projects, nodes, and settings |
| **Agent** | Rust (axum, bollard) | Runs on worker nodes, manages containers via mTLS |
| **Caddy** | Caddy | Reverse proxy with auto-TLS, dynamic routing via admin API |
| **CLI** | Rust (`l8b`) | Build and deploy apps from your terminal |
| **Database** | SQLite (WAL mode) | Lightweight, single-file persistence |
| **GitHub Action** | Bash | CI/CD auto-deploy on push |

## Infrastructure

| Feature | Logic Location |
|---|---|
| Auth / Users | Master (SQLite + axum-login) |
| Container Ops | Worker Agent (Rust + bollard) |
| Routing | Mode A: Master Caddy. Mode B: Distributed Caddy per node |
| Wake / Sleep | Mode A: Master waker. Mode B: Agent waker (autonomous) |
| DNS | Mode A: wildcard A record. Mode B: Cloudflare per-project A records |
| Agent config | Pushed from orchestrator over mTLS, no env vars needed |
| Node setup | Two-step: create node (pending) -> connect (health + push config -> online) |

## Routing Modes

### Mode A: Master Proxy (default)

All traffic flows through the master. Caddy proxies unknown subdomains to the orchestrator's wake handler. Remote agent traffic is encrypted with TLS (CA-verified) over the agent Caddy sidecar.

```
Browser -> Cloudflare (wildcard) -> Master Caddy --TLS--> Agent Caddy -> Container
```

**Note:** Master sees 2x bandwidth (proxies both directions). For high-traffic workloads, consider Cloudflare DNS mode.

### Mode B: Cloudflare DNS

DNS points directly to agent nodes. Each agent runs its own Caddy and handles wake locally. Works even when the master is down.

```
Browser -> Cloudflare (per-project A record) -> Agent IP -> Agent Caddy -> Agent waker
```

Hot-swappable from the dashboard. All agents must be reachable for Mode B.

See [multi-server.md](multi-server.md) for the full multi-server guide including bandwidth comparisons and manual DNS setup.

## Sleep & Wake

### Activity Tracking

The orchestrator tracks real HTTP traffic to update `last_active_at` in the database, so the janitor only stops truly idle projects — not ones actively serving requests.

**How it works:**

1. Caddy is configured with JSON access logs to stdout (`encoder.format: "json"`, `"logs": {}` on each server)
2. A background task tails the Caddy container's Docker log stream (`docker logs --follow`)
3. Each log line is parsed as JSON; `request.host` is extracted into a `HashSet` (deduped)
4. Every 60 seconds, the batch of unique hosts is flushed — one `UPDATE` query updates `last_active_at` for all matching running projects
5. Dashboard/poke hosts are filtered out; both subdomains (`myapp.l8b.in`) and custom domains (`app.example.com`) are matched

**Agent nodes** (Mode B): The agent tails its local Caddy logs and sends the batch to the orchestrator via `POST /internal/heartbeat` (HMAC-signed, same pattern as wake-report). The orchestrator handles the DB update for all nodes.

```
Mode A:  Caddy stdout → Docker stream → orchestrator tailer → DB UPDATE
Mode B:  Agent-Caddy stdout → Docker stream → agent tailer → HMAC POST → orchestrator → DB UPDATE
```

Memory overhead is minimal (~10-15 KB) — it's a single async task with a streaming reader and a small HashSet that resets every 60s.

### Janitor (background task)

Runs on a configurable interval (default: 5 min). Stops idle containers that haven't received traffic within their timeout threshold (default: 15 min). See [janitor.md](janitor.md) for the detailed flow.

### Waker (forward-auth handler)

When a request hits a sleeping app:

1. Return a loading page immediately
2. Check `auto_start_enabled` — if disabled, show "currently offline" page
3. Start the container in the background (single-flight dedup)
4. Rebuild Caddy routes
5. Browser auto-refreshes and hits the running container

In Mode B, the agent handles this autonomously using only the Docker API — no master or database needed.

See [waker.md](waker.md) for detailed wake-on-request flow diagrams.

### Custom Domain Wake

Sleeping custom domain wakes work identically in both modes via Caddy Host header rewrite. The orchestrator encodes sleeping custom domain routes with `host_rewrite` on `ProjectRoute` — Caddy rewrites the Host header from `app.example.com` to `myapp.l8b.in` before proxying to the local waker. The waker code is unchanged — it just extracts the subdomain.

### User-Facing Error Pages

All waker responses use consistent HTML templates (same in both modes):

| Page | Status | Shown when |
|---|---|---|
| Loading | 200 | Container is being started |
| Error | 503 | Container failed to start (30s auto-retry) |
| Offline | 503 | `auto_start_enabled` is disabled |
| Not Found | 404 | Project doesn't exist or was removed |

## Agent Independence (Mode B)

After one successful orchestrator push, an agent can operate fully independently:

| Capability | Works without master? |
|---|---|
| Wake sleeping containers (subdomain) | Yes |
| Wake sleeping containers (custom domain) | Yes — Host rewrite in persisted Caddy config |
| Route traffic to running containers | Yes — local Caddy rebuild from persisted config |
| Check `auto_start_enabled` before waking | Yes — project metadata persisted locally |
| Serve traffic after restart | Yes — persisted Caddy config + project metadata loaded on startup |
| Issue new TLS certificates | No — on-demand TLS needs orchestrator's `/caddy/ask` endpoint |
| Add new custom domains | No — done through dashboard (runs on orchestrator) |

### Agent Persistence

The agent persists three files to `data/` for restart resilience:

| File | Content | Updated by |
|---|---|---|
| `agent-state.json` | Node registration (node_id, secret, domain, wake_report_url) | `/internal/register` |
| `caddy-config.json` | Last orchestrator-pushed Caddy JSON config | `/caddy/sync` |
| `project-meta.json` | Project ID → `auto_start_enabled` mapping | `/internal/project-meta` |

### Caddy Config Merge (Agent Local Wake)

After waking a container, the agent rebuilds Caddy locally starting from the persisted orchestrator config as a base:

1. Take all non-catch-all routes from persisted config (sleeping custom domains, TLS config)
2. Add/update running container routes from Docker API (correct ports)
3. Upgrade sleeping custom domain routes for just-woken containers to direct proxy (no Host rewrite)
4. Append catch-all `*.{domain}` → agent wake handler
5. Push to Caddy + save updated config

### Project Metadata Push

The orchestrator pushes `auto_start_enabled` flags to agents via `POST /internal/project-meta`. Pushed on two triggers:

1. **Route sync** — covers deploy, stop, start, custom domain changes
2. **Settings toggle** — immediate push when `auto_start_enabled` is changed in dashboard

## Network

```
Host
+-- litebin-network (shared)
|   +-- orchestrator (5080, internal only)
|   +-- dashboard (internal only)
|   +-- caddy (80/443)
|   +-- app-1 (internal only)
|   +-- app-2 (internal only)
|   +-- agent (5083)
```

All services and app containers share a single Docker bridge network. Caddy routes traffic internally — no ports are exposed on the host except 80/443.

## mTLS (Master <-> Agent)

- Master holds a server cert signed by the Root CA
- Each agent holds a client cert signed by the same Root CA
- Both sides verify the certificate chain
- No HTTP fallback — mTLS is mandatory

```
Root CA (self-signed, 4096-bit RSA)
+-- Master server cert
+-- Node client cert (one per agent)
```

## Container Hardening

| Control | Value |
|---|---|
| Capabilities | `cap_drop: ALL`, `cap_add: CHOWN, DAC_OVERRIDE, SETGID, SETUID, NET_BIND_SERVICE, KILL` |
| Privilege escalation | `no-new-privileges` |
| Process limit | 4096 (fork bomb prevention) |
| Log rotation | 10 MB max, 3 files |
| Memory | 256 MiB default, per-project override |
| CPU | 0.5 cores default, per-project override |
| Network | Isolated bridge network |
| Restart policy | `no` (orchestrator manages lifecycle) |

## Database Schema

### `nodes`

| Column | Type | Description |
|---|---|---|
| id | TEXT PK | e.g. `local`, `node-eu-1` |
| name | TEXT | Human-friendly label |
| host | TEXT | IP or hostname |
| agent_port | INTEGER | Default 8443 |
| region | TEXT | Optional metadata |
| status | TEXT | `online`, `offline`, `draining`, `pending_setup` |
| total_memory | INTEGER | Bytes, reported by agent |
| total_cpu | REAL | Cores, reported by agent |
| last_seen_at | INTEGER | Unix timestamp of last heartbeat |
| fail_count | INTEGER | Consecutive missed heartbeats (>= 3 -> offline) |
| created_at | INTEGER | |
| updated_at | INTEGER | |

### `projects`

| Column | Type | Description |
|---|---|---|
| id | TEXT PK | Project ID, used as subdomain |
| user_id | TEXT FK | Owner |
| image | TEXT | Container image reference |
| internal_port | INTEGER | App's listening port |
| mapped_port | INTEGER | Host-mapped port (assigned at runtime) |
| container_id | TEXT | Docker container ID |
| node_id | TEXT FK | Which node this runs on |
| status | TEXT | `running`, `stopped`, `deploying`, `migrating` |
| last_active_at | INTEGER | Unix timestamp of last request |
| auto_stop_enabled | INTEGER | Janitor may stop when idle |
| auto_stop_timeout_mins | INTEGER | Idle threshold (default: 15) |
| auto_start_enabled | INTEGER | Waker may cold-start on visit |
| custom_domain | TEXT | Optional custom domain |
| cmd | TEXT | Custom container command |
| memory_limit_mb | INTEGER | Per-project memory limit |
| cpu_limit | REAL | Per-project CPU limit |
| created_at | INTEGER | |
| updated_at | INTEGER | |

Additional tables: `users`, `deploy_tokens`, `settings`.

See [security.md](security.md) for the full security architecture and threat model.
