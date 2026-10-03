# API Reference

## Orchestrator API

Base URL: `https://l8bin.example.com` (or `https://l8bin.localhost` locally)

### Authentication

Two methods:

- **Session cookie** (dashboard, `l8b login`) — full access to everything.
- **Deploy token** (`Authorization: Bearer <token>`) — scoped, cumulative access levels: `read < deploy < manage < admin`.

| Scope | Cumulatively allows |
|---|---|
| `read` | See state: project list/get, stats, logs, deploy-logs, disk-usage, node list, `/meta` |
| `deploy` | read + ship code: `/deploy`, `/deploy/compose`, `/compose/validate`, `/images/*` |
| `manage` | deploy + operate apps: stop/start/recreate, service ops, project/service settings, routes, capabilities, env writes |
| `admin` | manage + destructive/platform: project delete, volume deletes, node lifecycle, token CRUD, global settings |

Notes:

- Tokens are global or bound to a single project (`project_id`); project-bound tokens can only access that project's `/projects/{id}/…` paths (plus `/meta`).
- `admin` tokens are mintable only from a session (`POST /deploy-tokens` is session-only).
- Read-only tokens cannot deploy. Unauthenticated calls return `401` JSON.

---

### Auth

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/auth/login` | Public | Username/password login, creates session |
| `POST` | `/auth/logout` | Session | Clear session |
| `POST` | `/auth/register` | Public | Create first admin (only when no users exist) |
| `GET` | `/auth/me` | Session | Get current user info |
| `GET` | `/auth/setup` | Public | Check if initial admin setup is needed |
| `POST` | `/auth/change-password` | Session | Change current user's password |

### Deploy

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/deploy` | Session or token (deploy) | Create a new image deployment. Body includes `{project_id, image, is_background, port, ...}`. Web projects require `port`; background projects require it to be omitted. Returns `{status, project_id, url, ...}`, where `url` is `null` for background projects. |
| `PUT` | `/deploy` | Session or token (deploy) | Redeploy an existing project (upsert). Body: same as POST. Omitting `is_background` preserves an existing project's workload type. |

### Compose Deploy

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/deploy/compose` | Session or token (deploy) | Deploy a Compose project. Multipart includes `project_id`, `compose`, and optional `is_background`. The field explicitly sets workload type; omission preserves an existing type or defaults a new project to Web. Compose content never infers the type. Public-service selection is ignored for Background projects. Returns `{status, project_id, url}`, where `url` is `null` for Background projects. |
| `POST` | `/compose/validate` | Session or token (deploy) | Validate Compose compatibility for an explicit `is_background` value and return findings, required capabilities, missing grants, and the capability catalog. |

### Projects

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/projects` | Session | Create a new project (binds the session user) |
| `GET` | `/projects` | Session or token (read, global) | List all projects. Project-bound tokens get 403 — use a global token. |
| `GET` | `/projects/:id` | Session or token (read) | Get single project |

### Project Management

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/projects/:id/stop` | Session or token (manage) | Stop a running container (async) |
| `POST` | `/projects/:id/start` | Session or token (manage) | Start a stopped container |
| `DELETE` | `/projects/:id` | Session or token (admin) | Delete project + container + cleanup |
| `POST` | `/projects/:id/recreate` | Session or token (manage) | Remove and recreate container (picks up updated .env) |

### Runtime Env

Values are write-only: listings show keys with masked previews only, never plaintext.

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/projects/:id/env` | Session or token (read) | Env keys with masked previews + `pending_apply` flag (`.env` differs from what the container was last started with) |
| `PUT` | `/projects/:id/env` | Session or token (manage) | Update env. Body: `{env: {KEY: value}, delete: [KEY], mode: "merge"\|"replace"}` (default merge). Validates keys (`[A-Za-z_][A-Za-z0-9_]*`) and single-line values; applies on the next container start/recreate. |

### Service Management (multi-service)

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/projects/:id/services/:name/start` | Session or token (manage) | Start a specific service |
| `POST` | `/projects/:id/services/:name/stop` | Session or token (manage) | Stop a specific service |
| `POST` | `/projects/:id/services/:name/restart` | Session or token (manage) | Restart a specific service |
| `PATCH` | `/projects/:id/services/:name/settings` | Session or token (manage) | Update service settings: `{memory_limit_mb, cpu_limit}` |

### Volume Management

| Method | Path | Auth | Description |
|---|---|---|---|
| `DELETE` | `/projects/:id/volumes/:name` | Session or token (admin) | Delete a specific volume |
| `DELETE` | `/projects/:id/volumes` | Session or token (admin) | Delete all volumes for a project |

### Custom Routes

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/projects/:id/routes` | Session or token (manage) | List custom routes for a project |
| `POST` | `/projects/:id/routes` | Session or token (manage) | Create a custom route: `{route_type, path, subdomain, upstream, priority}` |
| `DELETE` | `/projects/:id/routes/:route_id` | Session or token (manage) | Delete a custom route |

### Project Settings

| Method | Path | Auth | Description |
|---|---|---|---|
| `PATCH` | `/projects/:id/settings` | Session or token (manage) | Update project settings: `{name, description, custom_domain, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit}` |

### Project Capabilities

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/projects/:id/capabilities` | Session or token (manage) | List capability status for a project |
| `POST` | `/projects/:id/capabilities` | Session or token (manage) | Grant capabilities: `{capabilities: ["docker-observe", "host-network"]}` |
| `DELETE` | `/projects/:id/capabilities/:capability` | Session or token (manage) | Revoke a capability and reconcile affected workloads |

### Project Stats & Logs

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/projects/stats` | Session or token (read, global) | Batch stats for all projects (`{"stats": [...]}`) |
| `GET` | `/projects/:id/stats` | Session or token (read) | Individual project stats |
| `GET` | `/projects/:id/disk-usage` | Session or token (read) | Disk usage for a project |
| `GET` | `/projects/:id/logs?tail=100&service=` | Session or token (read) | Container logs (proxied to agent for remote) |
| `GET` | `/projects/:id/deploy-logs` | Session or token (read) | Deploy logs |

### Images

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/images/upload?project_id=...&node_id=...` | Session or token (deploy) | Upload image tar as a single stream (legacy; local load or proxied to agent). Body: raw tar. |
| `POST` | `/images/upload-target` | Session or token (deploy) | Negotiate a chunked upload target. Body: `{project_id, image_id, node_id?, mode?}`. Returns `{mode, token, chunk_size, expires_at, base_url?, ca_pem?}` — `local`/`relay` target the master, `direct` returns the agent's public base URL + CA PEM. |
| `GET` | `/images/upload/{token}/status` | Token (in path) | Chunk indices the server has already received. |
| `POST` | `/images/upload/{token}/chunk/{index}` | Token (in path) | Upload one chunk (idempotent). Header `X-Total-Chunks`; body: raw chunk bytes. |
| `POST` | `/images/upload/{token}/commit` | Token (in path) | Assemble staged chunks and load (local) or stream to the agent (relay). Returns `{image_id}`. |

### Nodes

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/nodes` | Session or token (read) | List all nodes with status and load |
| `GET` | `/nodes/image-stats` | Session or token (read) | Image stats per node |
| `POST` | `/nodes` | Session or token (admin) | Create node (status: `pending_setup`). Returns node + agent_secret (shown once) |
| `POST` | `/nodes/:id/connect` | Session or token (admin) | Health check + push config via mTLS. Transitions to `online` |
| `DELETE` | `/nodes/:id` | Session or token (admin) | Decommission node (blocked if running projects) |
| `POST` | `/nodes/:id/images/prune` | Session or token (admin) | Prune dangling images on a node |

### Deploy Tokens

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/deploy-tokens` | Session | Create token (global or project-scoped, optional expiry, optional `scope`: read/deploy/manage/admin — default deploy). Returns plaintext (shown once) |
| `GET` | `/deploy-tokens?project_id=...` | Session | List deploy tokens (includes scope) |
| `DELETE` | `/deploy-tokens/:id` | Session | Revoke a token |

### Platform Meta & Global Settings

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/meta` | Session or token (read) | Non-sensitive platform metadata: `{domain, dashboard_subdomain, poke_subdomain, routing_mode, version}` — use for URL computation instead of `/settings` |
| `GET` | `/settings` | Session or token (admin) | Get global settings (includes Cloudflare credentials and `tryout` when domain is sslip/nip) |
| `PATCH` | `/settings` | Session or token (admin) | Update global settings (hot-swaps router if routing_mode changes). Domain must use `/settings/domain/apply`. Dashboard subdomain change syncs routes and re-registers agents. |
| `POST` | `/settings/domain/preflight` | Session or token (admin) | Validate a new platform domain (`{ domain }` → `{ ok, errors[], warnings[] }`) |
| `POST` | `/settings/domain/apply` | Session or token (admin) | Start domain change job (`{ domain, acknowledge_dns }` → `{ job_id }`) |
| `GET` | `/settings/domain/jobs/:id` | Session or token (admin) | Poll domain change job progress |
| `POST` | `/settings/domain/jobs/:id/retry` | Session or token (admin) | Retry a failed domain change job from the failed step |
| `POST` | `/settings/cleanup-dns` | Session or token (admin) | Delete all Cloudflare A records for the domain |
| `POST` | `/settings/sync-dns` | Session or token (admin) | Sync Cloudflare DNS records |

### Health

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/health` | Public | Orchestrator health (Docker ping + version) |
| `GET` | `/system/stats` | Session or token (admin) | System stats for stack services (memory, CPU, disk) |

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
| `POST` | `/containers/batch-run` | Deploy multi-service compose. Body: `{project_id, compose_yaml, service_order, target_services?}` |
| `POST` | `/containers/cleanup` | Full project cleanup: stop containers, remove volumes, remove network |

### Runtime Env

| Method | Path | Description |
|---|---|---|
| `GET` | `/internal/env?project_id=` | Raw `.env` content + `has_pending_changes` flag |
| `POST` | `/internal/env` | Replace the project's `.env`. Body: `{project_id, content}` (0600, ≤64KB) |

### Images

| Method | Path | Description |
|---|---|---|
| `POST` | `/images/load` | Load image from tar body. Returns `{image_id}` |
| `GET` | `/images/inspect?image=<ref>` | Resolve image reference (tag, digest, ID) to sha256 digest. Returns `{image_id}` |
| `POST` | `/images/remove-unused` | Remove image if not used by any container |
| `POST` | `/images/prune` | Prune all dangling images. Returns `{bytes_reclaimed}` |

### Direct upload (mTLS mint + token-gated chunk server)

The mint endpoint lives on the mTLS management port; the chunk endpoints are
served by a loopback listener reached by the agent's Caddy on `:443` via
`/__l8b_upload/*` (token-gated, no mTLS).

| Method | Path | Description |
|---|---|---|
| `POST` | `/internal/mint-upload-token` | mTLS. Body `{project_id, image_id, node_id, ttl_secs?}` → `{token, expires_at, chunk_size}`. Called by the master to set up a direct upload. |
| `GET` | `/__l8b_upload/{token}/status` | Chunk indices received so far. |
| `POST` | `/__l8b_upload/{token}/chunk/{index}` | Upload one chunk (idempotent). Header `X-Total-Chunks`. |
| `POST` | `/__l8b_upload/{token}/commit` | Assemble staged chunks and `docker load`. Returns `{image_id}`. |

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
