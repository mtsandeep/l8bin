# Scale-to-Zero — Waker & Janitor

LiteBin runs apps at zero cost when idle: the **janitor** stops containers no one is visiting, and the **waker** starts them again on the next request. This document covers both halves of that cycle.

```
visitor leaves → janitor stops the container after the idle timeout (minutes)
                     │
                     │  container kept on disk — fast `docker start` wake
                     ▼
visitor returns → Caddy catch-all → waker → container starts → loading page → app
```

---

# Part 1: Waker — Wake-on-Request

There are two waker implementations depending on routing mode:

| | Mode A (Master Proxy) | Mode B (Cloudflare DNS) |
|---|---|---|
| **Waker runs on** | Orchestrator | Agent (local) |
| **DB access** | Direct (same process) | None — discovers via Docker API |
| **Entry point** | `orchestrator/src/routes/waker/` | `agent/src/routes/waker/` |
| **Trigger** | Caddy catch-all / custom domain route → orchestrator | Caddy catch-all / custom domain route → agent |
| **Custom domain wake** | Caddy rewrites Host to `{id}.{domain}`, waker extracts subdomain | Same — Caddy rewrites Host, agent waker extracts subdomain |

In Mode A, all traffic goes through the master. In Mode B, DNS points directly to the agent node, so the agent handles the wake locally and reports back to master.

Both modes handle sleeping custom domain wakes identically: Caddy has an explicit route for each sleeping custom domain that rewrites the `Host` header to `{project_id}.{domain}` before proxying to the waker. The waker code is unchanged — it just extracts the subdomain from the rewritten Host header.

---

## Agent Registration

Agents no longer need `AGENT_NODE_ID`, `AGENT_SECRET`, `AGENT_DOMAIN`, or `AGENT_ORCHESTRATOR_URL` env vars. Instead:

1. Agent starts with only cert paths + port
2. Orchestrator creates node with `status = 'pending_setup'`
3. Heartbeat auto-detects agent is up, or user clicks "Connect" in dashboard
4. Orchestrator pushes config via `POST /internal/register` over mTLS: `{node_id, secret, domain, wake_report_url}`
5. Agent persists to `data/agent-state.json` for restarts
6. Node status set to `online`

This means agents are zero-config — just certs and they're ready.

---

## Mode A: Orchestrator Waker

### Idle State

- Project exists in DB with `status = "stopped"`, has a `container_id` and `mapped_port` from last run
- Caddy has **no** per-project route — only the catch-all `*.{domain} → orchestrator`
- If the project has a custom domain, Caddy has an explicit route: `{custom_domain} → orchestrator` with `Host` header rewritten to `{project_id}.{domain}` (and the www variant)

### Request Arrives

```
# Subdomain access
Browser → https://myapp.l8b.in → Cloudflare (wildcard) → Master Caddy

# Custom domain access (sleeping)
Browser → https://app.example.com → Cloudflare DNS → Master Caddy
```

**Subdomain**: Caddy has no specific route for `myapp.l8b.in`, so the catch-all `*.{domain}` matches → proxies to orchestrator → hits the `wake` handler.

**Custom domain**: Caddy matches the explicit sleeping custom domain route → rewrites `Host` to `myapp.l8b.in` → proxies to orchestrator → hits the `wake` handler. The waker extracts `myapp` from the rewritten Host header (same path as subdomain access).

### Waker Decision Tree (orchestrator)

```
wake() receives request with Host: myapp.l8b.in
  │
  ├─ Extract subdomain = "myapp"
  ├─ Look up project in DB
  │
  ├─ status="running" + mapped_port=Some?
  │   ├─ YES → is container actually alive?
  │   │   ├─ YES → verify port hasn't drifted, sync Caddy, return loading page
  │   │   └─ NO  → fall through to single-flight restart
  │   └─ NO → fall through
  │
  ├─ auto_start_enabled? NO → return 503
  │
  └─ Single-flight dedup (project_locks)
      ├─ Vacant (first request) → spawn background wake, return loading page immediately
      └─ Occupied (concurrent request) → completed+failed? return error page : return loading page
```

The first request returns the loading page **instantly** (503 with `Retry-After` header for API clients; HTML loading page for browsers). The Docker wake happens in a background task. The loading page has `<meta http-equiv="refresh" content="1">` so the browser re-requests every second.

### Background Wake — Local Project

```
start_stopped_container(state, project)
  │
  ├─ Compare .env hash vs .env.l8bin snapshot hash
  │   ├─ SAME → docker start (fast path, instant)
  │   │        └─ UPDATE projects SET status='running'
  │   └─ DIFFERENT → recreate path:
  │       ├─ Remove old container
  │       ├─ state.docker.run_container(project_clone, env)
  │       │   └─ Docker creates container with host_port="0" → auto-assigns new random port
  │       ├─ write .env.l8bin snapshot
  │       └─ UPDATE projects SET status='running', container_id=?, mapped_port=?
  │
  └─ route_sync_tx.send(()) → debounced route sync task rebuilds full Caddy config with per-project route:
      {host: "myapp.l8b.in"} → reverse_proxy → host.docker.internal:32771
```

See [Environment Secrets](../topics/env-secrets.md) for details on the `.env` / `.env.l8bin` snapshot system.

### Background Wake — Remote Project (Mode A)

```
start_stopped_container(state, project)
  │
  ├─ Look up agent node from DB (host, port)
  ├─ Build agent URL: https://{agent_host}:{agent_port}  (mTLS)
  │
  ├─ POST /containers/start {container_id, project_id, image, internal_port, ...}
  │   └─ Agent checks .env hash vs .env.l8bin snapshot:
  │       ├─ SAME → docker start → inspect port → return {mapped_port}
  │       └─ DIFFERENT → remove old, recreate with new env, return {mapped_port}
  │   ├─ SUCCESS → UPDATE projects, return Ok
  │   └─ FAIL (container pruned, agent restarted)
  │
  ├─ Try #2: POST /containers/recreate {image, port, project_id, ...}
  │   └─ Agent: remove old container (if exists)
  │            → docker create + start with host_port=0
  │            → inspect port → return {container_id, mapped_port}
  │   └─ UPDATE projects with new container_id + mapped_port
  │
  └─ route_sync_tx.send(()) → debounced route sync task: Caddy gets {host: "myapp.l8b.in"} → 10.0.1.5:32771
```

### Mode A Timeline

```
T+0s    Browser → Master Caddy → orchestrator wake()
        → Vacant entry → spawn background task
        → return loading page (instant)

T+0.5s  Background: docker rm old container
T+1s    Browser refresh → Occupied entry → loading page
T+2s    Background: docker create + start
T+3s    Browser refresh → Occupied entry → loading page
T+4s    Background: inspect port → UPDATE DB → route_sync_tx.send(())
        → debounced route sync task: Caddy now has: myapp.l8b.in → host.docker.internal:32771
        → lock removed

T+5s    Browser refresh → Caddy matches myapp.l8b.in
        → proxies directly to container → user sees app
```

---

## Mode B: Agent Waker

### Idle State

- Project exists in DB with `status = "stopped"`, has a `container_id` from last run
- Agent Caddy has **no** per-project subdomain route — only the catch-all `*.{domain} → agent wake handler`
- If the project has a custom domain, Caddy has an explicit route: `{custom_domain} → localhost:{agent_port}` with `Host` header rewritten to `{project_id}.{domain}` (and the www variant)
- Cloudflare DNS A record (`myapp.l8b.in → agent IP`) still exists (DNS isn't removed on stop)
- Sleeping custom domain DNS A records are also kept so the domain resolves to the agent

### Request Arrives

```
# Subdomain access
Browser → https://myapp.l8b.in → Cloudflare DNS (A record) → Agent IP → Agent Caddy

# Custom domain access (sleeping)
Browser → https://app.example.com → Cloudflare DNS → Agent IP → Agent Caddy
```

**Subdomain**: Caddy has no specific route for `myapp.l8b.in`, so the catch-all `*.{domain}` matches → proxies to agent's own wake handler (registered as `.fallback()`).

**Custom domain**: Caddy matches the explicit sleeping custom domain route → rewrites `Host` to `myapp.l8b.in` → proxies to agent wake handler. The agent waker extracts `myapp` from the rewritten Host header (same path as subdomain access). No DB or network call needed.

### Agent Waker Decision Tree

```
wake() receives request with Host: myapp.l8b.in
  │
  ├─ Extract subdomain = "myapp" (strips .{domain} suffix)
  ├─ Look up container by name: "litebin-myapp" (via Docker API, no DB)
  │   ├─ Not found → 404
  │   └─ Found → container_id
  │
  ├─ Container is running?
  │   └─ YES → rebuild local Caddy, return loading page
  │
  └─ Single-flight dedup (project_locks)
      ├─ Vacant (first request) → spawn background wake, return loading page
      └─ Occupied → completed+failed? error page : loading page
```

The agent waker does **not** check `auto_start_enabled` — it always tries to wake. The `auto_start_enabled` check is a master-side concern (DB field). If the container exists on the agent, it starts it.

### Background Wake — Agent (Autonomous)

```
wake() background task
  │
  ├─ Compare .env hash vs .env.l8bin snapshot hash
  │   ├─ SAME → fast path:
  │   │   ├─ docker.start_existing_container(container_id)
  │   │   └─ docker.inspect_mapped_port(container_id) → get mapped_port
  │   │
  │   └─ DIFFERENT → recreate path (reads metadata.json for container config):
  │       ├─ Read projects/<id>/metadata.json → {image, internal_port, cmd, ...}
  │       ├─ docker remove old container
  │       ├─ docker create + start with new env (host_port=0)
  │       ├─ write .env.l8bin snapshot
  │       └─ docker.inspect_mapped_port → get mapped_port
  │
  ├─ rebuild_local_caddy()
  │   ├─ Read persisted Caddy config (last orchestrator push) from data/caddy-config.json
  │   ├─ List ALL running litebin-* containers via Docker API
  │   ├─ Merge: keep orchestrator-pushed routes (TLS config, etc.)
  │   │         + add/update running container subdomain routes from Docker (correct ports)
  │   │         + upgrade sleeping custom domain routes for running containers to direct proxy
  │   │   {host: "myapp.l8b.in"}          → reverse_proxy → localhost:32771  (from Docker)
  │   │   {host: "other.l8b.in"}          → reverse_proxy → localhost:32772  (from Docker)
  │   │   {host: "app.example.com"}       → reverse_proxy → localhost:32771  (upgraded from sleeping CD)
  │   │   {host: "www.app.example.com"}   → reverse_proxy → localhost:32771  (upgraded from sleeping CD)
  │   │   {host: "sleeping.example.com"} → reverse_proxy → localhost:8443  (Host rewrite, still sleeping)
  │   │   {host: "*.{domain}"}            → reverse_proxy → localhost:{agent_port}  (wake handler)
  │   ├─ POST /load to local Caddy Admin API
  │   └─ Save updated config to data/caddy-config.json
  │
  └─ report_wake_to_master() (best-effort, fire-and-forget)
      ├─ POST https://poke.{domain}/internal/wake-report (HMAC-signed)
      │   Headers: X-Agent-Id, X-Agent-Timestamp, X-Agent-Signature
      │   Body: {project_id, container_id, mapped_port}
      ├─ Master: verify HMAC, UPDATE projects SET status='running', container_id=?, mapped_port=?
      └─ Master: send route_sync signal (debounced batch sync via background task)
```

**Agent project directory** (on the agent machine):
```text
projects/<id>/
├── .env            <-- runtime secrets (user edits this)
├── .env.l8bin      <-- env snapshot for change detection (auto-generated)
└── metadata.json   <-- container config for recreate without master (auto-generated)
```

The agent can wake and recreate containers fully autonomously using only these local files. See [Environment Secrets](../topics/env-secrets.md) for details on the env snapshot system.

**Key design:** The agent rebuilds Caddy **locally** starting from the last orchestrator-pushed config as a base. Sleeping custom domain routes and TLS config are preserved from the orchestrator push. Running container routes are added/updated from Docker API discovery. Sleeping custom domain routes for just-woken containers are automatically upgraded to direct proxy routes (no Host rewrite, correct port). No master or DB needed for the wake path. This means:

- Master down + agent up = agent wakes containers and serves traffic independently (subdomain + custom domain)
- Sleeping custom domain wakes work without master (Host rewrite in persisted Caddy config)
- Custom domain routing works immediately after local wake (upgraded from sleeping to running route)
- When master comes back, the wake-report or heartbeat reconciliation catches up the DB

### Mode B Timeline

```
T+0s    Browser → Cloudflare DNS → Agent IP → Agent Caddy
        → Catch-all → agent wake()
        → Vacant entry → spawn background task
        → return loading page (instant)

T+0.5s  Background: docker start {container_id}
T+1s    Browser refresh → Occupied entry → loading page
T+1.5s  Background: inspect port → 32771
T+2s    Background: list all running containers, rebuild Caddy config
        → POST /load to local Caddy
        → Caddy now has: myapp.l8b.in → localhost:32771
        → lock removed
T+2.5s  Background: POST /internal/wake-report to orchestrator (fire-and-forget)
        → Master: UPDATE DB, send debounced route sync signal

T+3s    Browser refresh → Agent Caddy matches myapp.l8b.in
        → proxies directly to container → user sees app
```

---

## Custom Domain Wake (Both Modes)

Sleeping custom domain wake works identically in both modes via Caddy Host header rewrite.

### How It Works

```
Request: app.example.com (sleeping app)
  ↓ DNS resolves to server IP
  ↓ Caddy matches explicit sleeping custom domain route
  ↓ Rewrites Host header: "app.example.com" → "myapp.l8b.in"
  ↓ Proxies to local waker (orchestrator or agent)
  ↓ Waker extracts "myapp" from Host → finds container → wakes it
```

The waker code is unchanged — it always extracts the subdomain from the Host header. The Host rewrite happens entirely in Caddy.

### Caddy Route Structure

**Sleeping custom domain** (both modes):
```json
{
  "match": [{ "host": ["app.example.com"] }],
  "handle": [{
    "handler": "reverse_proxy",
    "upstreams": [{ "dial": "litebin-orchestrator:3000" }],
    "headers": { "request": { "set": { "Host": ["myapp.l8b.in"] } } }
  }]
}
```

**Running custom domain** (no rewrite, direct proxy):
```json
{
  "match": [{ "host": ["app.example.com"] }],
  "handle": [{ "handler": "reverse_proxy", "upstreams": [{ "dial": "10.0.1.5:32771" }] }]
}
```

The switch from sleeping → running route happens automatically when `sync_routes()` runs after the wake completes. The orchestrator builds the correct routes based on project status.

### Www Variant

- **Sleeping**: `www.app.example.com` also has a Host rewrite route (same as canonical). No 301 redirect while sleeping.
- **Running**: `www.app.example.com` → 301 redirect to `app.example.com`.

---

## Agent Endpoints

| Endpoint | What it does |
|---|---|
| `POST /containers/start` | Smart start: compares `.env` vs `.env.l8bin` hash. Same → `docker start` (fast). Different → recreate with new env (reads `metadata.json`). Returns `{mapped_port}`. |
| `POST /containers/recreate` | Remove old, create fresh (no pull), reads `.env`, writes `.env.l8bin` snapshot + `metadata.json`, auto-assign port, return `{container_id, mapped_port}` |
| `POST /containers/run` | Pull image + create + start, auto-assign port, return `{container_id, mapped_port}` |
| `POST /caddy/sync` | Accept Caddy JSON config, persist to `data/caddy-config.json`, push to local Caddy Admin API |
| `POST /internal/register` | Receive config from orchestrator (mTLS auth): `{node_id, secret, domain, wake_report_url}`. Persists to `data/agent-state.json`. |
| `GET /health` | Report node resource usage, no registration needed |
| `fallback()` | Catch-all wake handler — starts stopped containers, rebuilds local Caddy |

## Orchestrator Internal Endpoints

| Endpoint | What it does |
|---|---|
| `POST /internal/wake-report` | Agent reports successful wake (HMAC-signed via `poke.{domain}`). Updates DB, sends debounced route sync signal. |
| `POST /internal/heartbeat` | Agent reports active hosts from access logs (HMAC-signed via `poke.{domain}`). Updates `last_active_at` for matching running projects. |
| `POST /nodes/{id}/connect` | Orchestrator pushes config to agent via mTLS: health check + register + set status to `online` |

---

## Failure Paths

### Background wake fails (either mode)

```
Background wake fails (Docker error, timeout, image gone)
  │
  ├─ guard.completed = true, guard.success = false
  ├─ Lock stays in project_locks (NOT removed)
  ├─ Auto-cleanup spawned: remove after 60s
  │
  Next browser refresh → Occupied entry
  ├─ completed=true, success=false → return error page
  │   "Failed to start myapp. Retrying in 30 seconds..."
  │
  After 60s → lock auto-removed → next refresh tries fresh wake
```

### Master is down during agent wake (Mode B)

```
Agent wakes container, rebuilds local Caddy
  │
  ├─ report_wake_to_master() → connection refused
  │   └─ Logged as debug, ignored (fire-and-forget)
  │
  ├─ Container is running, Caddy route exists locally
  └─ App serves traffic normally (agent is autonomous)

When master comes back up:
  ├─ Next agent heartbeat reports running containers
  ├─ Or next wake attempt succeeds at wake-report
  └─ Master reconciles DB state
```

---

## Edge Cases Handled

| Case | Mode A (Master) | Mode B (Agent) |
|---|---|---|
| Docker daemon restarts, port drifts | Waker inspects actual port, updates DB, resyncs Caddy | Container uses `docker start` (preserves port); if stale, rebuild catches it |
| Container pruned on agent | Remote start fails → falls back to recreate with stored image | Recreate from `metadata.json` + `.env` (no master needed) |
| Wake fails entirely | Error page with 30s retry; lock auto-clears after 60s | Same |
| Concurrent requests during wake | Single-flight dedup — all get instant loading page | Same |
| Background wake hangs | 60s timeout, then treated as failure | Same |
| Master is down | N/A (master IS the waker) | Agent handles locally, reports to master on recovery |
| Sleeping custom domain wake | Caddy Host rewrite → waker extracts subdomain → wakes | Same — Caddy Host rewrite → agent waker extracts subdomain |
| Www variant of sleeping custom domain | Same Host rewrite as canonical domain (no redirect while sleeping) | Same |
| Agent restarts | N/A | Loads persisted Caddy config from `data/caddy-config.json` immediately |
| Custom domain changed while agent down | N/A | Route updated on next heartbeat; DNS already points to agent |
| .env changed while container stopped | Detected on wake: hash mismatch → recreate with new env | Same — agent checks `.env` vs `.env.l8bin` locally |
| metadata.json missing on agent | N/A | Falls back to `docker start` (env changes not picked up until next deploy) |

---

## Caddy Config Persistence (Agent)

The agent persists the last orchestrator-pushed Caddy config to `data/caddy-config.json`. This solves two problems:

1. **Agent restart**: On startup, the persisted config is loaded and pushed to Caddy immediately. No gap waiting for orchestrator to push.

2. **Wake rebuild**: After waking a container, `rebuild_local_caddy()` uses the persisted config as a base instead of building from scratch. This preserves:
   - Sleeping custom domain routes (with Host rewrite)
   - TLS configuration (on-demand policies, `/caddy/ask` endpoint)
   - Any other orchestrator-managed routes

The merge logic:
- Take all non-catch-all routes from the persisted config
- Add/update running container subdomain routes from Docker API (correct ports)
- **Upgrade** sleeping custom domain routes for running containers: detect routes with `headers.request.set.Host` matching a running container's subdomain, replace with direct proxy (no Host rewrite, correct port)
- Keep sleeping custom domain routes for containers that are still stopped
- Append the catch-all `*.{domain}` → wake handler
- Push to Caddy + save updated config

When the orchestrator pushes a new config (via `/caddy/sync`), it replaces the persisted file entirely. This corrects any drift (e.g., removed custom domains, changed upstreams).

**Agent independence**: After one orchestrator push, the agent can operate fully independently. Subdomain and custom domain wake/routing work without master. TLS certs for already-issued domains continue to work. The only thing that requires master is adding NEW custom domains (done through the dashboard).

---

---

## Multi-Service Wake

Multi-service projects route directly to the public service container via Caddy (same as single-service). The waker is only involved when the container is down — Caddy's `handle_response` catches 502/503/504 and falls back to the orchestrator.

### Multi-Service Waker Decision Tree (Orchestrator)

```
request arrives for multi-service project
    ↓
Caddy routes to public service container directly
    ↓
if container is up → serve normally (all protocols supported)
if container is down → 502 → fallback to orchestrator waker
    ↓
if auto_start disabled → offline page
wake lock + start_services(force_recreate=false)
    ↓
loading page → next poll serves the site
```

### Why not proxy through the orchestrator?

Previously, all multi-service traffic went through the orchestrator so it could health-check services on every request. This broke WebSocket, gRPC, SSE, and other non-HTTP protocols. With direct Caddy→container routing, all protocols work. Service health is handled by Docker's `restart: unless-stopped` policy.

### Multi-Service Wake vs Single-Service Wake

| Aspect | Single-Service | Multi-Service |
|---|---|---|
| Caddy routes to | Container directly (when running) | Container directly (when running) |
| Wake trigger | 502 fallback → orchestrator | 502 fallback → orchestrator |
| Service health | Docker restart policy | Docker restart policy |

---

---

# Part 2: Janitor — Idle Detection & Auto-Stop

The janitor is a background task that stops projects which have been idle beyond their configured timeout. It only stops projects that are truly idle — not ones actively serving requests.

## Overview

```
                  ┌──────────────────────┐
                  │  Caddy access logs   │
                  │  (JSON, stdout)      │
                  └──────────┬───────────┘
                             │ Docker log stream
                             ▼
                  ┌──────────────────────┐
                  │  Activity Tracker    │
                  │  (log tailer task)   │
                  │  Batch every 60s     │
                  └──────────┬───────────┘
                             │ UPDATE projects SET last_active_at
                             ▼
                  ┌──────────────────────┐
                  │  SQLite (projects)   │
                  │  last_active_at      │
                  └──────────┬───────────┘
                             │ Queried every 5 min
                             ▼
                  ┌──────────────────────┐
                  │  Janitor             │
                  │  Stops idle projects │
                  └──────────────────────┘
```

## Activity Tracking (last_active_at Updates)

### Mode A: Master Proxy

The orchestrator runs a single activity tracker that tails the master Caddy container's logs.

```
Caddy (master)
  ├─ Access log per request → stdout (JSON)
  └─ Docker log stream → orchestrator log tailer
       ├─ Parse each line, extract request.host
       ├─ Collect unique hosts in HashSet (deduped)
       └─ Every 60s → UPDATE projects SET last_active_at = now
            WHERE status = 'running'
              AND auto_stop_enabled = 1
              AND (id IN (...) OR custom_domain IN (...))
```

**Host matching:**

| Log host | Classification | SQL match |
|---|---|---|
| `myapp.l8b.in` | Subdomain → `id = "myapp"` | `WHERE id IN ('myapp')` |
| `app.example.com` | Custom domain | `WHERE custom_domain IN ('app.example.com')` |
| `l8b.in` | Dashboard | Skipped |
| `poke.l8b.in` | Internal poke endpoint | Skipped |

### Mode B: Cloudflare DNS

Each agent tails its own Caddy container's logs and reports to the orchestrator.

```
Agent Caddy
  ├─ Access log per request → stdout (JSON)
  └─ Docker log stream → agent log tailer
       ├─ Parse each line, extract request.host
       ├─ Collect unique hosts in HashSet
       └─ Every 60s → POST /internal/heartbeat (HMAC-signed)
            Body: { "hosts": ["myapp.l8b.in", "app.example.com"] }
            Headers: X-Agent-Id, X-Agent-Timestamp, X-Agent-Signature

Orchestrator
  └─ /internal/heartbeat handler
       ├─ Verify HMAC signature (same pattern as wake-report)
       └─ Run same UPDATE query as Mode A
```

The orchestrator handles all DB updates for all nodes. Agents only forward raw hostnames.

### Caddy Config

All Caddy configs (master + agent) include:

```json
{
  "logging": {
    "logs": {
      "default": {
        "writer": { "output": "stdout" },
        "encoder": { "format": "json" }
      }
    }
  },
  "apps": {
    "http": {
      "servers": {
        "srv0": {
          "logs": {},
          ...
        }
      }
    }
  }
}
```

- `encoder.format: "json"` — structured logs with `request.host` field for parsing
- `writer.output: "stdout"` — Docker captures stdout; no custom Caddy modules needed
- `logs: {}` on the server — explicitly enables access log emission (required in Caddy v2.10+)

### Log Format

Each HTTP request produces one JSON line:

```json
{
  "level": "info",
  "ts": 1775828248.373,
  "logger": "http.log.access",
  "msg": "handled request",
  "request": {
    "host": "myapp.l8b.in",
    "method": "GET",
    "uri": "/",
    ...
  },
  "status": 200,
  "duration": 0.00166,
  ...
}
```

The activity tracker extracts `request.host`, strips the port if present, and adds it to the batch.

### Performance & Memory

| Aspect | Detail |
|---|---|
| Per-request cost | JSON parse (~1-5us) + HashSet insert |
| DB writes | 1 UPDATE per 60s per node (batched, not per-request) |
| Memory | ~10-15 KB (one async task + streaming reader + small HashSet) |
| Restart resilience | Auto-reconnects if the log stream breaks (container restart, etc.) |
| Deduplication | Same host hit 1000 times in 60s = 1 DB update |

## Janitor (Auto-Stop)

### Configuration

| Setting | Default | Location |
|---|---|---|
| Janitor interval | 5 minutes | Orchestrator env: `JANITOR_INTERVAL_SECS` |
| Auto-stop timeout | 15 minutes | Per-project: `auto_stop_timeout_mins` |
| Auto-stop enabled | Yes (opt-in per project) | Per-project: `auto_stop_enabled` |

### Decision Flow

```
Janitor runs every 5 minutes
  │
  ├─ SELECT * FROM projects
  │   WHERE status = 'running'
  │     AND auto_stop_enabled = 1
  │     AND last_active_at < (now - auto_stop_timeout_mins)
  │
  ├─ For each idle project (local or via the node agent over mTLS):
  │   ├─ docker stop {container_id} — the container is KEPT (fast `docker start` wake);
  │   │   only the Docker-observation proxy sidecar (if present) is removed
  │   ├─ UPDATE projects SET status = 'stopped'  (container_id kept)
  │   └─ Send route_sync signal → Caddy removes the per-project route
  │       (DNS sync is skipped — no Cloudflare API calls during periodic checks)
  │
  └─ Log: "janitor: container stopped (idle)" per project
```

### Timeline Example

```
T+0min   User visits myapp.l8b.in
         → Activity tracker records host, batch pending

T+1min   Activity tracker flushes batch
         → UPDATE projects SET last_active_at = now WHERE id = 'myapp'

T+2min   User visits again
         → last_active_at updated again

...no more visits...

T+17min  Janitor runs
         → SELECT ... WHERE last_active_at < (now - 15 min)
         → myapp's last_active_at is 15 min old → IDLE
         → docker stop → status = 'stopped' (container kept)
         → Caddy route removed

T+18min  New visitor hits myapp.l8b.in
         → Caddy catch-all → waker → container starts → loading page → auto-refresh → app served
```

### What Triggers last_active_at Updates

| Event | Updates last_active_at? |
|---|---|
| HTTP request to running app (any status code) | Yes (activity tracker) |
| Deploy / redeploy | Yes (deploy code sets it) |
| Wake from sleep | Yes (wake code sets it) |
| Container start (manual) | Yes (start code sets it) |
| Janitor stopping a container | No (status changes to 'stopped') |

### Edge Cases

| Case | Behavior |
|---|---|
| Project with `auto_stop_enabled = 0` | Never stopped by janitor, regardless of idle time |
| Project with `auto_start_enabled = 0` | Stopped normally by janitor; won't auto-wake on visit (shows offline page) |
| No activity tracker running | `last_active_at` only updates on deploy/wake/start; janitor still works but uses stale timestamps |
| Activity tracker misses a flush (restart) | Lost hosts in that 60s window; next flush catches up. Worst case: project gets stopped 1-2 min earlier than ideal |
| Agent can't reach orchestrator for heartbeat | Fire-and-forget; logged as debug. Janitor may stop project slightly earlier than it should on that node |
| Multiple agents reporting same host | HashSet deduplication within each node; DB UPDATE handles overlapping host lists via IN clause |

---

---

## Further Reading

- [Architecture](architecture.md) — full system overview, component responsibilities
- [Multi-Service Architecture](../topics/multi-service.md) — per-project networks, dependency levels, internal routing
- [Multi-Server Setup](../topics/multi-server.md) — adding agents, routing modes, certificate architecture
- [Design Decisions](decisions.md) — why mTLS, why two routing modes, why scale-to-zero
- [Failure Model](failure-model.md) — how wake failures, master down, and port drift are handled
- [Security](../topics/security.md) — threat model and mTLS architecture
- [User Flows](user-flows.md) — all user-triggerable scenarios and their current status
