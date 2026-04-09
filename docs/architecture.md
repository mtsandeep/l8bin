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

All traffic flows through the master. Caddy proxies unknown subdomains to the orchestrator's wake handler.

```
Browser -> Cloudflare (wildcard) -> Master Caddy -> Orchestrator (wake handler)
```

### Mode B: Cloudflare DNS

DNS points directly to agent nodes. Each agent runs its own Caddy and handles wake locally. Works even when the master is down.

```
Browser -> Cloudflare (per-project A record) -> Agent IP -> Agent Caddy -> Agent waker
```

Hot-swappable from the dashboard. All agents must be reachable for Mode B.

## Sleep & Wake

### Janitor (background task)

Runs on a configurable interval (default: 5 min). Stops idle containers that haven't received traffic within their timeout threshold (default: 15 min).

### Waker (forward-auth handler)

When a request hits a sleeping app:

1. Return a loading page immediately
2. Start the container in the background (single-flight dedup)
3. Rebuild Caddy routes
4. Browser auto-refreshes and hits the running container

In Mode B, the agent handles this autonomously using only the Docker API — no master or database needed.

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
See [waker.md](waker.md) for detailed wake-on-request flow diagrams.
