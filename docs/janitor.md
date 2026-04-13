# Janitor Flow — Idle Detection & Auto-Stop

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
  ├─ For each idle project:
  │   ├─ Mode A (local node):
  │   │   ├─ docker stop {container_id}
  │   │   ├─ docker rm {container_id}
  │   │   ├─ UPDATE projects SET status = 'stopped', container_id = NULL
  │   │   └─ Send route_sync signal → Caddy removes per-project route
  │   │       (DNS sync is skipped — no Cloudflare API calls during periodic checks)
  │   │
  │   └─ Mode B (remote node):
  │       ├─ POST /containers/stop {container_id} → agent (mTLS)
  │       ├─ UPDATE projects SET status = 'stopped', container_id = NULL
  │       └─ Send route_sync signal → Caddy removes per-project route
  │           + push updated config to agent
  │           (DNS sync is skipped — no Cloudflare API calls during periodic checks)
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
         → docker stop → docker rm → status = 'stopped'
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
