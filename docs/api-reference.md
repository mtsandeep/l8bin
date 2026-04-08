# API Reference

## Orchestrator API

Base URL: `https://l8bin.example.com` (or `http://localhost:5080` locally)

### Authentication

Most endpoints require session auth (from `l8b login`). The deploy and image upload endpoints also accept deploy tokens via `Authorization: Bearer <token>`.

---

### Auth

| Method | Path | Description |
|---|---|---|
| `POST` | `/auth/login` | Username/password login, creates session |
| `POST` | `/auth/logout` | Clear session |
| `POST` | `/auth/register` | Create first admin (only when no users exist) |
| `GET` | `/auth/me` | Get current user info |
| `GET` | `/auth/setup-check` | Check if initial admin setup is needed |
| `POST` | `/auth/change-password` | Change current user's password |

### Deploy

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/deploy` | Session or token | Deploy a container. Body: `{project_id, image, port, name, description, node_id, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, custom_domain}`. Returns `{status, project_id, url, custom_domain, mapped_port}` |

### Projects

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/projects` | Session | Create a new project |
| `GET` | `/projects` | Public | List all projects |
| `GET` | `/projects/:id` | Public | Get single project |

### Project Management

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/projects/:id/stop` | Session | Stop a running container (async) |
| `POST` | `/projects/:id/start` | Session | Start a stopped container |
| `DELETE` | `/projects/:id` | Session | Delete project + container + cleanup |
| `POST` | `/projects/:id/recreate` | Session | Remove and recreate container (picks up updated .env) |

### Project Settings

| Method | Path | Auth | Description |
|---|---|---|---|
| `PATCH` | `/projects/:id/settings` | Session | Update project settings: `{name, description, custom_domain, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit}` |

### Project Stats & Logs

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/projects/stats` | Session | Batch stats for all projects |
| `GET` | `/projects/:id/stats` | Session | Individual project stats |
| `GET` | `/projects/:id/disk-usage` | Session | Disk usage for a project |
| `GET` | `/projects/:id/logs?tail=100` | Session | Container logs (proxied to agent for remote) |

### Images

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/images/upload?project_id=...&node_id=...` | Session or token | Upload image tar (local or proxied to agent). Body: raw tar. |

### Nodes

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/nodes` | Session | List all nodes with status and load |
| `POST` | `/nodes` | Session | Create node (status: `pending_setup`). Returns node + agent_secret (shown once) |
| `POST` | `/nodes/:id/connect` | Session | Health check + push config via mTLS. Transitions to `online` |
| `DELETE` | `/nodes/:id` | Session | Decommission node (blocked if running projects) |
| `GET` | `/nodes/image-stats` | Session | Image stats per node |
| `POST` | `/nodes/:id/images/prune` | Session | Prune dangling images on a node |

### Deploy Tokens

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/deploy-tokens` | Session | Create token (global or project-scoped, optional expiry). Returns plaintext (shown once) |
| `GET` | `/deploy-tokens?project_id=...` | Session | List deploy tokens |
| `DELETE` | `/deploy-tokens/:id` | Session | Revoke a token |

### Global Settings

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/settings` | Session | Get global settings |
| `PUT` | `/settings` | Session | Update global settings (hot-swaps router if routing_mode changes) |
| `POST` | `/settings/cleanup-dns` | Session | Delete all Cloudflare A records for the domain |

### Health

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | Public | Orchestrator health (Docker ping + version) |
| `GET` | `/system-stats` | Session | System stats for stack services (memory, CPU, disk) |

### Caddy

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/caddy/ask?domain=<fqdn>` | Public | On-Demand TLS validation. Returns 200 if domain belongs to a known project |

### Waker

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `*.{domain}` (catch-all) | Public | Wake handler. Returns loading page, starts container in background |

### Wake Report (Internal)

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/internal/wake-report` | mTLS + HMAC | Agent reports successful wake. HMAC-SHA256 signed with 5-min replay protection |

---

## Agent API

All agent endpoints are mTLS-protected (no application-level auth). The orchestrator communicates with agents over mTLS.

### Containers

| Method | Path | Description |
|---|---|---|
| `POST` | `/containers/run` | Pull image + create + start. Returns `{container_id, mapped_port}` |
| `POST` | `/containers/recreate` | Remove old + create fresh (no pull). Returns `{container_id, mapped_port}` |
| `POST` | `/containers/start` | Start an existing stopped container. Returns `{mapped_port}` |
| `POST` | `/containers/stop` | Stop a container |
| `POST` | `/containers/remove` | Remove a container |
| `GET` | `/containers/:id/status` | Inspect container status, port, CPU/memory |
| `GET` | `/containers/:id/logs?tail=100` | Stream container logs |
| `GET` | `/containers/:id/disk-usage` | Disk usage for a container |
| `POST` | `/containers/stats` | Batch stats. Body: `{container_ids: [...]}` |

### Images

| Method | Path | Description |
|---|---|---|
| `POST` | `/images/load` | Load image from tar body. Returns `{image_id}` |
| `POST` | `/images/remove-unused` | Remove image if not used by any container |
| `POST` | `/images/prune` | Prune all dangling images. Returns `{bytes_reclaimed}` |

### Health

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Node resource usage (memory, CPU, disk, container count) |

### Caddy

| Method | Path | Description |
|---|---|---|
| `POST` | `/caddy/sync` | Accept full Caddy JSON config, push to local Caddy |

### Registration (Internal)

| Method | Path | Description |
|---|---|---|
| `POST` | `/internal/register` | Receive config from orchestrator: `{node_id, secret, domain, wake_report_url}` |

### Volumes

| Method | Path | Description |
|---|---|---|
| `POST` | `/volumes/export` | Export volume (not yet implemented) |
| `POST` | `/volumes/import` | Import volume (not yet implemented) |

### Waker (Agent-side)

| Method | Path | Description |
|---|---|---|
| `GET` | `*` (catch-all fallback) | Agent wake handler. Finds container by subdomain via Docker API, starts it, rebuilds local Caddy, reports wake to master |
