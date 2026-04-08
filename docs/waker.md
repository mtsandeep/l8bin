# Waker Flow — Wake-on-Request Architecture

There are two waker implementations depending on routing mode:

| | Mode A (Master Proxy) | Mode B (Cloudflare DNS) |
|---|---|---|
| **Waker runs on** | Orchestrator | Agent (local) |
| **DB access** | Direct (same process) | None — discovers via Docker API |
| **Entry point** | `orchestrator/src/routes/waker.rs` | `agent/src/routes/waker.rs` |
| **Trigger** | Caddy catch-all → orchestrator | Caddy catch-all → agent |

In Mode A, all traffic goes through the master. In Mode B, DNS points directly to the agent node, so the agent handles the wake locally and reports back to master.

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

### Request Arrives

```
Browser → https://myapp.l8b.in → Cloudflare (wildcard) → Master Caddy
```

Caddy has no specific route for `myapp.l8b.in`, so the catch-all `*.{domain}` matches → proxies to orchestrator → hits the `wake` handler.

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
  └─ Single-flight dedup (wake_locks)
      ├─ Vacant (first request) → spawn background wake, return loading page immediately
      └─ Occupied (concurrent request) → completed+failed? return error page : return loading page
```

The first request returns the loading page **instantly**. The Docker wake happens in a background task. The loading page has `<meta http-equiv="refresh" content="1">` so the browser re-requests every second.

### Background Wake — Local Project

```
start_stopped_container(state, project)
  │
  ├─ Remove old container (stale port binding)
  ├─ state.docker.run_container(project_clone, env)
  │   └─ Docker creates container with host_port="0" → auto-assigns new random port
  ├─ Get back (new_container_id, new_mapped_port)
  ├─ UPDATE projects SET status='running', container_id=?, mapped_port=?
  └─ route_sync_tx.send(()) → debounced route sync task rebuilds full Caddy config with per-project route:
      {host: "myapp.l8b.in"} → reverse_proxy → host.docker.internal:32771
```

Next browser refresh → Caddy matches `myapp.l8b.in` → proxies directly to container. Done.

### Background Wake — Remote Project (Mode A)

```
start_stopped_container(state, project)
  │
  ├─ Look up agent node from DB (host, port)
  ├─ Build agent URL: http://10.0.1.5:5081
  │
  ├─ Try #1: POST /containers/start {container_id}
  │   └─ Agent: docker start {container_id} → inspect port → return {mapped_port}
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
- Agent Caddy has **no** per-project route — only the catch-all `*.{domain} → agent wake handler`
- Cloudflare DNS A record (`myapp.l8b.in → agent IP`) still exists (DNS isn't removed on stop)

### Request Arrives

```
Browser → https://myapp.l8b.in → Cloudflare DNS (A record) → Agent IP → Agent Caddy
```

Caddy has no specific route for `myapp.l8b.in`, so the catch-all `*.{domain}` matches → proxies to agent's own wake handler (registered as `.fallback()`).

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
  └─ Single-flight dedup (wake_locks)
      ├─ Vacant (first request) → spawn background wake, return loading page
      └─ Occupied → completed+failed? error page : loading page
```

The agent waker does **not** check `auto_start_enabled` — it always tries to wake. The `auto_start_enabled` check is a master-side concern (DB field). If the container exists on the agent, it starts it.

### Background Wake — Agent (Autonomous)

```
wake() background task
  │
  ├─ docker.start_existing_container(container_id)
  │   └─ Preserves original port binding (docker start, not create)
  ├─ docker.inspect_mapped_port(container_id) → get mapped_port
  │
  ├─ rebuild_local_caddy()
  │   ├─ List ALL running litebin-* containers via Docker API
  │   ├─ Inspect port for each
  │   ├─ Build Caddy config with per-project routes + catch-all
  │   │   {host: "myapp.l8b.in"} → reverse_proxy → localhost:32771
  │   │   {host: "other.l8b.in"} → reverse_proxy → localhost:32772
  │   │   {host: "*.{domain}"}   → reverse_proxy → localhost:{agent_port}  (wake handler)
  │   └─ POST /load to local Caddy Admin API
  │
  └─ report_wake_to_master() (best-effort, fire-and-forget)
      ├─ POST https://poke.{domain}/internal/wake-report (HMAC-signed)
      │   Headers: X-Agent-Id, X-Agent-Timestamp, X-Agent-Signature
      │   Body: {project_id, container_id, mapped_port}
      ├─ Master: verify HMAC, UPDATE projects SET status='running', container_id=?, mapped_port=?
      └─ Master: send route_sync signal (debounced batch sync via background task)
```

**Key design:** The agent rebuilds Caddy **locally** using only the Docker API. No master or DB needed. This means:

- Master down + agent up = agent wakes containers and serves traffic independently
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

## Agent Endpoints

| Endpoint | What it does |
|---|---|
| `POST /containers/start` | `docker start` existing container, inspect port, return `{mapped_port}` |
| `POST /containers/recreate` | Remove old, create fresh (no pull), auto-assign port, return `{container_id, mapped_port}` |
| `POST /containers/run` | Pull image + create + start, auto-assign port, return `{container_id, mapped_port}` |
| `POST /caddy/sync` | Accept Caddy JSON config, push to local Caddy Admin API |
| `POST /internal/register` | Receive config from orchestrator (mTLS auth): `{node_id, secret, domain, wake_report_url}`. Persists to `data/agent-state.json`. |
| `GET /health` | Report node resource usage, no registration needed |
| `fallback()` | Catch-all wake handler — starts stopped containers, rebuilds local Caddy |

## Orchestrator Internal Endpoints

| Endpoint | What it does |
|---|---|
| `POST /internal/wake-report` | Agent reports successful wake (HMAC-signed via `poke.{domain}`). Updates DB, sends debounced route sync signal. |
| `POST /nodes/{id}/connect` | Orchestrator pushes config to agent via mTLS: health check + register + set status to `online` |

---

## Failure Paths

### Background wake fails (either mode)

```
Background wake fails (Docker error, timeout, image gone)
  │
  ├─ guard.completed = true, guard.success = false
  ├─ Lock stays in wake_locks (NOT removed)
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
| Container pruned on agent | Remote start fails → falls back to recreate with stored image | Container not found → 404 (user needs to redeploy) |
| Wake fails entirely | Error page with 30s retry; lock auto-clears after 60s | Same |
| Concurrent requests during wake | Single-flight dedup — all get instant loading page | Same |
| Background wake hangs | 60s timeout, then treated as failure | Same |
| Master is down | N/A (master IS the waker) | Agent handles locally, reports to master on recovery |
