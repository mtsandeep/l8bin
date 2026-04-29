# Multi-Service Architecture

How LiteBin handles docker-compose projects with multiple containers — per-project networking, internal routing, dependency-ordered startup, and health checks.

---

## Overview

LiteBin supports both single-service (one container) and multi-service (docker-compose with multiple containers) projects. Multi-service projects get their own isolated Docker network, level-by-level startup respecting `depends_on`, and direct container routing via Caddy (same as single-service).

All running projects — single and multi-service — get direct Caddy→container routes. If the container is down, Caddy's `handle_response` catches the 502/503/504 and falls back to the orchestrator for auto-wake. This avoids proxying every request through the orchestrator, which would break WebSocket, gRPC, SSE, and other non-HTTP protocols.

---

## Per-Project Networks

Each multi-service project gets its own Docker bridge network:

```
litebin-{project_id}    (e.g., litebin-myapp)
```

### Who connects to the network

| Component | Why |
|---|---|
| **Caddy** | Routes external traffic directly to the public service container |
| **Orchestrator** | Auto-wake fallback (502/503/504 → orchestrator → wake → retry) |
| **Each service container** | DNS-based service discovery between services |

### DNS aliases

Each service container gets:
- **Hostname**: service name (e.g., `web`, `api`, `db`)
- **DNS alias** in the network: service name (e.g., `web` resolves to the container's network IP)

This means services can reach each other using just the service name:
```
web → http://api:8080/health
api → postgres://db:5432/mydb
```

### Network lifecycle

| Event | Action |
|---|---|
| Deploy / Start | `ensure_project_network` — creates if not exists (idempotent) |
| Orchestrator restart | `connect_to_project_networks` — scans all `litebin-*` networks, reconnects |
| Project delete | `remove_project_network` — tears down the network |

---

## Public Service

Every multi-service project has at most one **public service** — the service that receives external HTTP traffic. Detection priority:

1. Service with label `litebin.public=true`
2. Service exposing port 80 or 443 (if exactly one)
3. Service exposing any port (if exactly one)
4. None — internal-only projects allowed

Only the public service gets a host port binding. Non-public services are internal-only on the Docker network.

---

## Internal Service Routing

### Caddy route logic

All running projects route directly to their container:

| Project type | Caddy routes to |
|---|---|
| Single-service (running) | Direct to container upstream (`litebin-myapp:3000`) |
| Multi-service (running) | Direct to public service container (`litebin-myapp.web:3000`) |
| Any (stopped/crashed) | 502 → fallback to orchestrator for auto-wake |

### Why direct routing

Previously, multi-service projects routed through the orchestrator so it could health-check all services on every request. This broke WebSocket, gRPC, SSE, and other non-HTTP protocols because the orchestrator's proxy (reqwest) only supports standard HTTP.

With direct Caddy→container routing, all protocols work natively. Service health is handled by:

- **Docker restart policy** (`restart: unless-stopped`) — crashed containers auto-restart
- **Caddy 502 fallback** — if the public service container is down, Caddy falls back to the orchestrator which wakes all services
- **Dashboard** — manual start/stop/recreate of individual services

### Inter-service communication

Services within the same project communicate via the Docker network using service names:
```
web container → http://api:8080/api/data
api container → postgres://db:5432/appdb
```

No ports are exposed on the host for internal services. Caddy only knows about the public service.

---

## Service Dependency Levels

Services are started in order based on `depends_on` in compose.yaml.

### Topological sort

Uses BFS-based topological sort (Kahn's algorithm):

1. Compute in-degree for each service (count of dependencies)
2. Services with in-degree 0 form **level 0** (no dependencies)
3. Removing level 0 reduces in-degrees; services reaching in-degree 0 form **level 1**
4. Repeat until all services are assigned

Example:
```
Level 0:  db, redis          (no dependencies)
Level 1:  api                (depends on db, redis)
Level 2:  web                (depends on api)
```

### Parallel within level, sequential between levels

```
start level 0:  [db] ─────┐
                 [redis] ──┤  parallel (JoinSet)
                            ↓  wait for all to complete
start level 1:  [api] ─────┤
                            ↓
start level 2:  [web] ─────┘
```

### Dependency conditions

Compose `depends_on` supports conditions:

| Condition | Behavior |
|---|---|
| `service_started` (default) | Wait for container to start |
| `service_healthy` | Wait for Docker healthcheck to report healthy |
| `service_completed_successfully` | Wait for container to exit with code 0 |

When a downstream service depends on another with `service_healthy`, the orchestrator polls Docker inspect every 500ms for up to 60 seconds.

### Rollback on failure

When `rollback_on_failure` is true (deploy only), if any service fails to start, all previously started containers in the project are stopped and removed.

---

## Health Checks

### Docker restart policy (recommended)

Services with `restart: unless-stopped` in their compose file will automatically restart if they crash. This is the primary health recovery mechanism for multi-service projects:

```yaml
services:
  api:
    image: myapp
    restart: unless-stopped
  db:
    image: postgres:16
    restart: unless-stopped
```

Without a restart policy, crashed containers stay down until manually restarted from the dashboard.

### Degraded state recovery

When non-public services are down but the public service is healthy, traffic continues to flow normally. Crashed services with `restart: unless-stopped` will recover automatically via Docker. For services without a restart policy, use the dashboard to restart them individually.

### Startup health checks

After each container starts during `start_services`:
1. **Network readiness**: Polls every 200ms for up to 10s until Docker assigns a valid IP
2. **Docker healthcheck** (if required by downstream): Polls every 500ms for up to 60s for `HEALTHY` status

---

## Database: `project_services` Table

```sql
CREATE TABLE project_services (
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_name    TEXT NOT NULL,
    image           TEXT NOT NULL,
    port            INTEGER,
    cmd             TEXT,
    is_public       INTEGER NOT NULL DEFAULT 0,
    depends_on      TEXT,                    -- JSON array: '["db","redis"]'
    container_id    TEXT,
    mapped_port     INTEGER,
    memory_limit_mb INTEGER,
    cpu_limit       REAL,
    status          TEXT NOT NULL DEFAULT 'stopped',  -- deploying|running|stopped|error
    instance_id     TEXT DEFAULT NULL,
    PRIMARY KEY (project_id, service_name)
);
```

Denormalized fields on `projects` for the fast health-check path (no JOINs needed):
- `service_count INTEGER DEFAULT 1` — number of services
- `service_summary TEXT` — e.g., `"db:api:web"`

---

## Compose Parsing

### Supported fields

| Field | Support |
|---|---|
| `image` / `build` | String or object form |
| `command` / `entrypoint` | String or list form |
| `environment` | Map or list form (compose env takes precedence over LiteBin env overrides) |
| `ports` | List of strings (`"8080"`, `"80:3000"`, `"9090/udp"`) |
| `depends_on` | List form or map form with conditions |
| `volumes` | Supported |
| `healthcheck` | List form and string form (`CMD`/`CMD-SHELL`) |
| `labels` | Supported (including `litebin.public=true`) |
| `memory` / `cpus` | Supported (`"512m"`, `"1g"`, `"1.5"`) |
| `cap_add` / `cap_drop` | Supported |
| `read_only` / `tmpfs` / `extra_hosts` | Supported |

Unknown fields are silently captured (`#[serde(flatten)]`) for forward compatibility.

### Variable interpolation

Variable interpolation is supported: `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, `$VAR`, and `$$` (escaped). Variables are resolved from the compose `environment` section, `.env` files, and system environment variables.

### Validation pipeline

Four checks run in order on deploy:

1. **Ghost dependencies** — all `depends_on` references must point to services in the same compose file
2. **Cycle detection** — Kahn's algorithm; if topological sort can't include all services, a cycle exists
3. **Topological sort** — produces flat start order (also validates DAG)
4. **Public service detection** — returns error if multiple services have `litebin.public=true`

---

## Unified `start_services()` Function

The orchestrator has a single `start_services()` function in `multi_service.rs` that handles all container startup scenarios:

```rust
pub async fn start_services(
    state: &AppState,
    project_id: &str,
    compose: &ComposeFile,
    opts: StartServicesOpts,
) -> Result<(), Response>

pub struct StartServicesOpts {
    pub force_recreate: bool,        // deploy/recreate → true, wake/dashboard → false
    pub pull_images: bool,            // deploy → true, others → false
    pub services: Option<HashSet<String>>,  // selective recreate, None = all
    pub connect_orchestrator: bool,   // always true
    pub rollback_on_failure: bool,    // deploy → true, others → false
}
```

### Behavior

```
For each service level (parallel within level):
    if force_recreate:
        stop + remove existing container
        create new container
    else:
        if container exists and running → skip
        if container exists and stopped → docker start
        if container doesn't exist (or was deleted) → create new
        if docker start fails (stale DB) → remove stale, create new
```

### Callers

| Caller | force_recreate | pull_images | services | rollback |
|---|:---:|:---:|---|:---:|
| Wake (auto-start) | false | true | None | false |
| Dashboard start project | false | false | None | false |
| Dashboard start service | true | false | Some({svc}) | false |
| Dashboard restart service | true | false | Some({svc}) | false |
| Dashboard recreate | true | false | Some/None | false |
| Deploy compose | true | false | None | true |

---

## Unified Project Lock

All container-modifying operations share a single lock per project:

```rust
pub project_locks: Arc<DashMap<String, Arc<Semaphore>>>
```

This replaces the old separate `wake_locks` and `deploy_locks` mechanisms. Operations that acquire the lock:

- Wake (auto-start)
- Deploy / Redeploy
- Recreate (full or selective)
- Start / Stop / Restart
- Janitor auto-sleep
- Multi-service health check recovery

The waker uses `try_acquire_project_lock` (non-blocking) — if the lock is held by another operation (e.g., a deploy), the waker returns the loading page and lets the browser retry.

---

## Container Lifecycle State Machine

```
                    ┌──────────────┐
                    │   CREATED    │
                    │  (running)   │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │  STOPPED │ │ DEGRADED │ │  ERROR   │
        │(user stop│ │(some svc │ │(wake     │
        │ janitor) │ │ crashed) │ │ failed)  │
        └────┬─────┘ └────┬─────┘ └────┬─────┘
             │            │            │
             ▼            ▼            ▼
        ┌──────────────────────────────────┐
        │        START_SERVICES            │
        │  force_recreate=false (wake)     │
        │  force_recreate=true  (deploy)   │
        └──────────────────────────────────┘
```

---

## Key Invariants

1. **`project_services.status` always reflects reality** — every flow that touches containers updates the DB.
2. **One function handles all start scenarios** — no duplication between waker, dashboard, and deploy.
3. **`docker start` is always tried first** (unless `force_recreate`). On failure, falls back to recreate.
4. **Network is always connected** before starting containers.
5. **DNS wait only when needed** — skipped when all containers were just started (not created).

---

## Further Reading

- [Architecture](architecture.md) — full system overview, component responsibilities, database schema
- [Waker](waker.md) — detailed wake-on-request flow diagrams for both single and multi-service
- [Multi-Server Setup](multi-server.md) — adding agents, routing modes, certificate architecture
- [Design Decisions](decisions.md) — why Rust, Caddy, SQLite, Docker (not K8s)
- [Failure Model](failure-model.md) — how every component handles failures and recovery
- [User Flows](user-flows.md) — all user-triggerable scenarios and their current status
