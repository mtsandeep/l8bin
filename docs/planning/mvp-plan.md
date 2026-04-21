# Multi-Service MVP Plan

Core multi-service implementation. After this, users can deploy multi-service projects via compose file, services are isolated on per-project networks, volumes persist, lifecycle works, and scale-to-zero works.

**Prerequisite:** [Pre-MVP Plan](pre-mvp-plan.md) (waker 503, volume persistence, custom routes)

**Compose parsing:** [compose-bollard](compose-bollard-crate.md) — internal crate that converts compose YAML to bollard Docker API config structs

**Implementation approach:** All phases ship together as one release. The phases below are for organizing the doc, not incremental deliveries.

---

## Design Principles

### Unified Code Path

Every project internally uses `project_services`. Single-service is just multi-service with one service. The deploy endpoint is the **only** branching point:

```rust
let services: Vec<ProjectService> = match payload {
    DeployFormat::SingleImage(json) => {
        // Normalize single-image into one service
        vec![ProjectService {
            service_name: "web".into(),
            image: json.image,
            port: Some(json.port),
            is_public: true,
            ..Default::default()
        }]
    }
    DeployFormat::Compose(file) => {
        parse_compose(file)?
    }
};

// One code path from here:
deploy_services(&state, project_id, services).await
```

### Minimal DB, Compose is Source of Truth

The DB stores only what **LiteBin needs for runtime and routing**. Docker config fields (`entrypoint`, `working_dir`, `user`, `shm_size`, `tmpfs`, `read_only`, `extra_hosts`, `env`) are read from the stored compose file at deploy/wake time — not duplicated into the DB.

| In DB | Why |
|---|---|
| `project_id`, `service_name` | Identity |
| `image` | Display + re-pulls |
| `port` | Caddy routing |
| `is_public` | Caddy routing |
| `depends_on` | Startup ordering |
| `cmd` | From deploy request (single-service) |
| `memory_limit_mb`, `cpu_limit` | LiteBin-managed resource limits |
| `container_id`, `mapped_port`, `status` | Runtime state |

| Read from compose file | When |
|---|---|
| `entrypoint`, `working_dir`, `user`, `shm_size`, `tmpfs`, `read_only`, `extra_hosts`, `env` | Deploy and wake (re-parse compose) |

No compose file (single-service) = no compose fields. Simple.

### Migration

Existing single-service projects are migrated via a simple INSERT SELECT:

```sql
INSERT INTO project_services (project_id, service_name, image, port, is_public,
                              container_id, mapped_port, status)
SELECT id, 'web', image, internal_port, 1, container_id, mapped_port, status
FROM projects WHERE image IS NOT NULL;
```

After migration, the `projects` table's `image`, `internal_port`, `container_id`, `mapped_port` columns become denormalized caches (still written for the 5s poll, but `project_services` is the source of truth).

---

## Data Model

### Schema

```sql
-- Services within a project (minimal — only what LiteBin needs)
CREATE TABLE project_services (
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_name    TEXT NOT NULL,
    image           TEXT NOT NULL,
    port            INTEGER,
    cmd             TEXT,
    is_public       INTEGER NOT NULL DEFAULT 0,
    depends_on      TEXT,           -- JSON: ["db", "redis"]
    container_id    TEXT,
    mapped_port     INTEGER,
    memory_limit_mb INTEGER,
    cpu_limit       REAL,
    status          TEXT DEFAULT 'stopped',
    PRIMARY KEY (project_id, service_name)
);

-- Volume definitions per service
CREATE TABLE project_volumes (
    project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_name   TEXT NOT NULL,
    volume_name    TEXT,           -- directory name under projects/{id}/data/ (defaults to service_name)
    container_path TEXT NOT NULL,  -- path inside the container
    PRIMARY KEY (project_id, service_name, container_path)
);

-- Denormalized fields on projects for the 5s poll (no JOINs)
ALTER TABLE projects ADD COLUMN service_count INTEGER DEFAULT 1;
ALTER TABLE projects ADD COLUMN service_summary TEXT;
-- service_summary format: "web:3000, db:5432, cache:6379"
```

### Project Status Aggregation

The `projects.status` field reflects the aggregate state of all services:

| Condition | Project Status |
|---|---|
| All services running | `running` |
| Some running, some stopped | `degraded` (new status) |
| All stopped | `stopped` |
| Any service starting/deploying | `starting` / `deploying` |
| Any service error | `error` |

**Routing:** `resolve_routes()` filters by `status = 'running'`. Extend to include `degraded` when the public service has a valid `mapped_port`.

**Janitor:** Auto-stop triggers only when ALL services are idle. `last_active_at` updated when ANY service receives traffic.

### Stats Query

Stats endpoint gets container IDs via UNION:

```sql
SELECT container_id FROM projects WHERE id = ? AND container_id IS NOT NULL
UNION ALL
SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL
```

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/db/migrations/0015_multi_service.sql` | New tables + migration INSERT + ALTER TABLE |
| `litebin-common/src/types.rs` | `ProjectService`, `ProjectVolume` structs |
| `orchestrator/src/db/models.rs` | Re-export new types |

---

## Docker: Networks + run_service_container

### Per-Project Isolated Networks

Create a Docker bridge network per multi-service project. Single-service projects keep using `litebin-network` (unchanged).

```
# Single-service (existing, no change):
litebin-network
└── litebin-myapp              → public via Caddy

# Multi-service (new):
litebin-myapp (bridge)
├── web (alias: "web")         → litebin-myapp-web     → public via Caddy (also on litebin-network)
└── db  (alias: "db")          → litebin-myapp-db      → internal only
```

**Dual-network for public services:** Public service connects to both `litebin-network` (Caddy) AND `litebin-{project_id}` (inter-service DNS). Internal services connect to `litebin-{project_id}` only.

```
Single-service:        network_mode = "litebin-network"                    (no change)
Multi-service public:  networking_config = { litebin-network + litebin-{project_id} }
Multi-service internal: networking_config = { litebin-{project_id} only }
```

**Resource overhead:** ~2-5 KB per project network. Negligible.

**DNS aliases:** Set via `EndpointSettings.aliases` when connecting a container to the per-project network. Services reach each other by short name: `db:5432`, `cache:6379`.

### run_service_container

The single container creation method used by all code paths. Replaces the old `run_container`:

```rust
async fn run_service_container(&self, config: &RunServiceConfig) -> Result<(String, u16)>
```

`RunServiceConfig` includes:
- `project_id`, `service_name`, `image`, `port`, `cmd`
- `entrypoint`, `working_dir`, `user` (from compose file at deploy/wake time)
- `env` (HashMap — project `.env` + compose `environment:` merged at deploy time)
- `memory_limit_mb`, `cpu_limit` (from DB)
- `shm_size`, `tmpfs`, `read_only`, `extra_hosts` (from compose file at deploy/wake time)
- `networks` (list of network names + DNS aliases)
- `binds` (list of bind mount strings)

### DockerManager Methods

```rust
/// Create a per-project bridge network (idempotent).
async fn ensure_project_network(&self, project_id: &str) -> anyhow::Result<String>

/// Remove a per-project network (only if no containers connected).
async fn remove_project_network(&self, project_id: &str) -> anyhow::Result<()>

/// Ensure project data directory exists.
fn ensure_data_dir(projects_base: &str, project_id: &str, volume_name: &str) -> std::io::Result<PathBuf>

/// Read and parse stored compose file for a project.
fn read_compose(projects_base: &str, project_id: &str) -> anyhow::Result<Option<ComposeFile>>
```

### Security Model

LiteBin is self-hosted — the user owns the server and the compose file. No compose validation for security. The only security guarantee is **architectural isolation**:

```
Internet → Caddy (litebin-network) → web container (on both networks)
                                       └→ db:5432 (per-project network only)
                                          └→ NOT reachable from Caddy
                                          └→ NOT reachable from internet
```

Enforced by:
1. Per-project Docker networks — internal services only on `litebin-{project_id}`
2. Caddy routing — only public service gets a Caddy route
3. No host port mapping — LiteBin controls port bindings, not the compose file

> **Note:** To expose an internal service (e.g., a database admin panel), the user marks it with `litebin.public: "true"` in the compose file and redeploys. This prevents accidental leakage — exposing a service is always intentional. A future improvement could add a zero-downtime toggle via `docker network connect/disconnect` without redeploy.

### Files Modified

| File | Change |
|---|---|
| `litebin-common/src/docker.rs` | `run_service_container` (replaces `run_container`), `ensure_project_network`, `remove_project_network`, `ensure_data_dir` |
| `litebin-common/src/types.rs` | `RunServiceConfig` struct |

---

## Compose Deploy

### Compose Handling

Handled by the [compose-bollard](compose-bollard-crate.md) internal crate. It converts compose YAML into bollard Docker API config structs with a generic default mapping. LiteBin overrides only orchestration-specific fields.

```rust
use compose_bollard::{ComposeParser, BollardMappingOptions};

// 1. Parse compose file
let compose = ComposeParser::parse(&compose_yaml)?;

// 2. Validate (4 LiteBin-logic checks)
let order = compose.topological_sort()?;
let public = compose.detect_public_service()?;

// 3. Get bollard config (generic mapping — all compose fields → bollard types)
let mut config = compose.get_service("web")?.to_bollard_config(&BollardMappingOptions {
    env_overrides: project_env,  // project .env merged in
    auto_tmpfs_for_readonly: true,
})?;

// 4. Override orchestration-specific fields
config.host_config.binds = Some(litebin_binds);         // project data dirs
config.host_config.port_bindings = Some(litebin_ports); // LiteBin-controlled
config.networking_config = Some(litebin_networks);     // per-project + dual-network
// Everything else (entrypoint, shm_size, user, cap_add, etc.) flows through from compose
```

### LiteBin Overrides (4 fields)

| Override | Why |
|---|---|
| `binds` | Project data directory bind mounts (`projects/{id}/data/`) |
| `port_bindings` | LiteBin controls port allocation, not compose |
| `networking_config` | Per-project network + dual-network for public services |
| `env` | Project `.env` merged with compose `environment:` |

Everything else is handled by compose-bollard's generic mapping. When the crate adds support for new compose fields, LiteBin gets them automatically.

### Compose Field Mapping

| docker-compose Field | Stored In | Mapped By |
|---|---|---|
| `services.web.image` | DB (`project_services.image`) | LiteBin |
| `services.web.ports` | DB (`project_services.port`) | LiteBin (first port only) |
| `services.web.depends_on` | DB (`project_services.depends_on`) | LiteBin (topological sort) |
| `services.web.command` | DB (`project_services.cmd`) | LiteBin |
| `services.web.labels` | LiteBin (`is_public` detection) | LiteBin |
| `services.web.environment` | compose-bollard (merged with project .env) | compose-bollard + LiteBin override |
| All other Docker config fields | compose-bollard (generic mapping) | compose-bollard |
| `volumes.db_data` | DB (`project_volumes`) | LiteBin |

### Deploy API

Two formats, same endpoint:

```json
// Format 1: Single service (backward compatible, normalized to one service internally)
POST /deploy
{ "project_id": "my-app", "image": "ghcr.io/me/myapp:latest", "port": 3000 }
```

```bash
# Format 2: Multi-service (compose file upload)
POST /deploy
Content-Type: multipart/form-data
compose_file: @docker-compose.yml
project_id: my-app
```

Detection: if `compose_file` is present in multipart request, parse as compose. Otherwise, existing single-image JSON.

### env_file Handling

The agent already reads `projects/{project_id}/.env` via `dotenvy` and injects all vars. Strip `env_file` from compose during parsing. Merge order:
1. Project `.env` (base, shared across all services)
2. Compose `environment:` per service (overrides project `.env`)

### Validation (LiteBin Logic Only — 4 Checks)

No compose validation for security (self-hosted). Only validate what would break LiteBin's own logic:

| Check | Why | Error |
|---|---|---|
| Circular dependencies | Topological sort would loop | `"Circular dependency detected: {cycle}"` |
| `depends_on` references non-existent service | Code would try to start non-existent service | `"Service '{svc}' depends on '{dep}' which does not exist"` |
| No public service detected | LiteBin needs to know what to route Caddy to | `"No public service found. Add \`litebin.public: \"true\"\` label or expose a port."` |
| Multiple public services | One Caddy route per project | `"Multiple public services detected. Only one service can be public."` |

Everything else — let Docker validate and return Docker's error.

### Public Service Detection

Priority order:
1. Service with `litebin.public: "true"` label
2. Service with `ports` mapped to `80` or `443`
3. Service with `expose` and no other service depends on it
4. First service defined in compose (fallback)

### Volume Cleanup on Redeploy

- **Default: preserved** (no data loss on redeploy)
- **Opt-in: deleted** via `cleanup_volumes: true` flag in deploy request
- Log orphaned volume paths in deploy response

### Compose File Storage

Stored in project data directory. Re-read at deploy and wake time to get Docker config fields:

```
projects/myapp/
├── .env              ← shared env vars
├── compose.yml       ← stored compose file (re-parsed at deploy/wake)
└── data/             ← bind mount targets
```

### Batch Agent Request

Send a single batch request with all services instead of N sequential requests. Agent pulls all images in parallel, then starts containers in dependency order.

```
Current:  [pull image] → [start container]
Multi:    [pull all images in parallel] → [start in dependency order]
```

### Full Deploy Flow

```
1. Acquire per-project deploy lock
2. Detect format (single-image JSON or compose multipart)
3. If compose: ComposeParser::parse(), run 4 validation checks, store compose.yml
4. Normalize to Vec<ProjectService> — store only LiteBin-needed fields in DB
5. Compute dependency graph via topological sort (compose-bollard)
6. Detect public service (compose-bollard)
7. Pull all images in parallel
8. Create per-project network (if multi-service)
9. Create all data directories under projects/{id}/data/
10. Re-parse compose.yml via ComposeParser to get bollard config for each service
11. Start services in topological order:
    For each service:
      a. Get bollard config from compose-bollard (generic mapping)
      b. Override binds, port_bindings, networking_config, env (LiteBin-specific)
      c. Create container (run_service_container with bollard config)
      d. Start container
      e. Store container_id and mapped_port in project_services
12. On failure: roll back all created containers (reverse order)
13. Update project status (aggregated from services)
14. Update service_count and service_summary on projects table
15. Sync Caddy (route to public service only)
```

### Files Modified

| File | Change |
|---|---|
| `crates/compose-bollard/` | New internal crate — compose YAML → bollard config mapping |
| `orchestrator/src/routes/deploy.rs` | Format detection, normalization, multi-service deploy flow, batch agent request |
| `agent/src/routes/containers.rs` | Batch run endpoint |
| `litebin-common/src/types.rs` | `RunBatchRequest`, `RunBatchResponse` |

---

## Lifecycle (Start/Stop/Delete/Recreate)

All lifecycle operations loop over `project_services`. One code path for single and multi-service.

### Operations

| Operation | Flow |
|---|---|
| **Start** | Read `compose.yml` → query `project_services` → start in dependency order |
| **Stop** | Query `project_services` → stop in reverse dependency order |
| **Delete** | Stop all, remove all containers, remove per-project network, optionally remove data dirs |
| **Recreate** | Read `compose.yml` → remove all, recreate in dependency order (no image pull) |

Start and recreate re-parse `compose.yml` to get Docker config fields. Stop and delete don't need it.

**Delete confirmation:** For multi-service projects, warn that `projects/{id}/data/` will be removed. Require explicit confirmation.

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/routes/manage.rs` | Multi-service start/stop/delete/recreate loops |
| `agent/src/routes/containers.rs` | Batch start/stop/remove endpoints |

---

## Routing (Caddy to Public Service)

Route Caddy to the public service container. Internal services get no Caddy entry.

Custom routes (path-based and subdomain-based) are already handled by [pre-MVP Feature 3](pre-mvp-plan.md#feature-3-custom-routing-rules). The upstream naming `litebin-{project_id}-{service}:{port}` already matches multi-service container names.

### Upstream Naming

```
Single-service:  litebin-{project_id}:{port}           (unchanged)
Multi-service:   litebin-{project_id}-{service}:{port}  (public service)
```

For **local nodes**, upstream resolved on `litebin-network` via container name (public service is on both networks via dual-network).

For **remote nodes**, upstream is `{host}:443` with `upstream_tls = true` — agent's local Caddy handles final routing.

### Custom Routes + Multi-Service

Custom route upstreams can only target services on `litebin-network` (public services). Internal services (per-project network only) are not reachable by Caddy:

```
# Works — public service (on litebin-network via dual-network)
litebin-myapp-web:3000

# Works — public service of another project
litebin-otherapp:3000

# Doesn't work — internal service (per-project network only)
litebin-myapp-db:5432
```

To expose an internal service as a custom route target, mark it with `litebin.public: "true"` and redeploy.

### Changes

- `resolve_routes()`: For multi-service projects (service_count > 1), look up public service in `project_services`, use `litebin-{project_id}-{service}:{port}` as upstream (custom routes already handled by pre-MVP)
- Status filter: Include `degraded` status (public service up, some internals may be down)
- Agent `rebuild_local_caddy()`: Same upstream naming change

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/routing_helpers.rs` | Public service lookup, upstream naming |
| `agent/src/routes/waker.rs` | `rebuild_local_caddy()` — same upstream naming |

---

## Agent (Waker + Janitor)

Wake handler starts all services for a project. Janitor stops all services when idle. Both handle multi-service via `project_services`.

### Wake + Caddy Sync Race Fix

**Problem:** Waker starts containers in dependency order. Route sync is debounced at 500ms. If sync fires before all services start, Caddy routes to a non-existent backend → 502.

**Fix:** Signal route sync only after all services are started.

```
Single-service:
  start container → signal route sync

Multi-service (per-service status updates at each step):
  db: starting    → db: running     →
  redis: starting → redis: running  →
  web: starting   → web: running    → signal route sync
```

For multi-service projects, the waker:
1. Checks `project_services` to determine if multi-service
2. Reads `compose.yml` to get Docker config fields
3. Starts all services in dependency order, updating each service's `status` as it transitions (`starting` → `running` / `error`)
4. Only then triggers route sync signal
5. Updates aggregated project status
6. Reports wake to master

### Janitor Changes

- Query `project_services` for the project
- Stop all services in reverse dependency order, updating each service's `status` as it transitions (`stopping` → `stopped`)
- Mark project as `stopped` only after all services are stopped
- Activity tracking: `last_active_at` updated when ANY service receives traffic

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/routes/waker.rs` | Multi-service wake flow |
| `agent/src/routes/waker.rs` | Multi-service wake + Caddy sync |
| `orchestrator/src/sleep/janitor.rs` | Multi-service stop loop |
| `orchestrator/src/activity.rs` | Multi-service container ID query |

---

## Container Naming Convention

| Context | Container Name | Network |
|---|---|---|
| Single-service (existing) | `litebin-{project_id}` | `litebin-network` |
| Multi-service | `litebin-{project_id}-{service}` | `litebin-{project_id}` (+ `litebin-network` if public) |
| Preview (future) | `litebin-{project_id}-pr-{number}-{service}` | `litebin-{project_id}-pr-{number}` |

---

## Resource Impact Summary

For a VPS running 20 projects (10 single-service, 10 multi-service with 3 services each):

| Resource | Current | After MVP | Delta |
|---|---|---|---|
| RAM (LiteBin) | ~15-20 MB | ~15-20 MB | 0 MB |
| RAM (Docker networks) | ~5 KB | ~55 KB | +50 KB |
| Disk (LiteBin) | ~5 MB | ~5.1 MB | +100 KB |
| Disk (project data) | 0 B | User-dependent | User-controlled |
| Deploy time (single) | ~5s | ~5s | 0s |
| Deploy time (multi, 3 svc) | N/A | ~10-15s | N/A |
| Wake time (single) | 1-3s | 1-3s | 0s |
| Wake time (multi, 3 svc) | N/A | 5-10s | N/A |

---

## Implementation Checklist

### Data Model + Migration
- [ ] Create migration `0015_multi_service.sql` with `project_services`, `project_volumes` tables (minimal schema)
- [ ] Migration: INSERT SELECT to normalize existing projects
- [ ] ALTER TABLE: add `service_count`, `service_summary` to projects
- [ ] `ProjectService`, `ProjectVolume` structs in `litebin-common/src/types.rs`
- [ ] Project status aggregation function
- [ ] Stats UNION ALL query

### Docker
- [ ] `ensure_project_network()` on DockerManager
- [ ] `remove_project_network()` on DockerManager
- [ ] `ensure_data_dir()` on DockerManager
- [ ] `run_service_container()` replacing `run_container` — all Docker fields passed via `RunServiceConfig`
- [ ] `RunServiceConfig` struct

### Compose Deploy
- [ ] Create `crates/compose-bollard/` — ComposeParser, serde structs, bollard mapping, validation helpers
- [ ] v1 mappings: image, command, entrypoint, working_dir, user, environment, ports, depends_on, volumes, shm_size, tmpfs, read_only, extra_hosts, memory, cpus, cap_add/drop, labels
- [ ] Override pattern: LiteBin sets binds, port_bindings, networking_config, env
- [ ] Deploy format detection (single-image JSON vs compose multipart)
- [ ] Normalize single-image to Vec<ProjectService>
- [ ] Compose file storage + re-read at deploy/wake time
- [ ] Batch agent request (parallel pulls, sequential starts)
- [ ] env_file stripping

### Lifecycle
- [ ] Multi-service start (re-parse compose, dependency order)
- [ ] Multi-service stop (reverse order)
- [ ] Multi-service delete (with data dir warning)
- [ ] Multi-service recreate (re-parse compose, dependency order)
- [ ] Batch agent endpoints for start/stop/remove

### Routing
- [ ] `resolve_routes()` multi-service upstream naming
- [ ] Status filter includes `degraded`
- [ ] Agent `rebuild_local_caddy()` upstream naming

### Agent/Waker/Janitor
- [ ] Multi-service wake flow (re-parse compose, start all, then sync)
- [ ] Multi-service janitor stop flow (reverse order)
- [ ] Activity tracking across all services

---

## Open Issues to Resolve During Implementation

### 1. Stats Query Duplicate container_ids After Migration

The stats UNION ALL query reads from both `projects` and `project_services`:

```sql
SELECT container_id FROM projects WHERE id = ? AND container_id IS NOT NULL
UNION ALL
SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL
```

After migration, existing projects have the same `container_id` in both tables. UNION ALL returns duplicates, causing double-counted stats.

**Fix:** After migration, read stats only from `project_services`:

```sql
SELECT container_id FROM project_services WHERE project_id = ? AND container_id IS NOT NULL
```

The `projects.container_id` becomes a denormalized cache for the 5s poll display only — never used for stats.

### 2. Compose File Must Reach the Agent (Mode B Wake)

In Mode B (Cloudflare DNS), the agent handles wake directly. It needs to re-read `compose.yml` to get Docker config fields. But the compose file is uploaded to the orchestrator during deploy.

**Fix:** The orchestrator includes the compose file content in the batch deploy request. The agent stores it locally in `projects/{id}/compose.yml`. Both orchestrator and agent can then read it when needed.

```json
// Batch agent request
{
  "project_id": "myapp",
  "compose_yaml": "...(full compose file content)...",
  "services": [
    { "service_name": "web", "image": "...", "port": 3000, ... }
  ]
}
```

### 3. Single-Service Doesn't Support Docker Config Fields

Single-service deploys (no compose file) don't get Docker config fields like `entrypoint`, `shm_size`, `working_dir`, etc. These only come from compose.

**This is by design.** Single-service is basic. Need advanced Docker config? Use compose. Not a bug — a feature boundary to document.

### 4. run_container Callers Must All Migrate

`run_service_container` replaces `run_container`. All callers must be updated: deploy (local + remote), start, recreate, wake. Since all phases ship together, this is a one-time migration with no backward compat needed.
