# Pre-MVP Plan

Standalone improvements that ship before multi-service. Every item here adds value to existing single-service users today and is a prerequisite or enabler for multi-service.

---

## Feature 1: Waker 503 + JSON for API Clients

### Problem

The waker returns `200 OK` with HTML for every request — including JSON API clients, webhooks, and CLI tools. A `curl` to a sleeping API gets back HTML, not JSON.

### Fix

Check the `Accept` header in both the orchestrator waker and the agent waker:

```rust
let accept = headers.get("accept")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("");

if accept.contains("text/html") {
    // Browser — return HTML loading page with auto-refresh
    (StatusCode::OK, Html(loading_page_html(project_id)))
} else {
    // Everything else — 503 JSON with retry hint
    (StatusCode::SERVICE_UNAVAILABLE, [
        (header::RETRY_AFTER, "5"),
    ], Json(json!({"error": "starting", "retry_after": 5})))
}
```

### Behavior

| Client | Request | Response |
|---|---|---|
| Browser | `Accept: text/html` | `200` HTML + meta refresh |
| `curl` / `fetch` | `Accept: */*` (no html) | `503` JSON `{"error": "starting", "retry_after": 5}` |
| Webhook (GitHub, Stripe) | `Accept: application/json` | `503` JSON — sender retries on 5xx |
| Mobile app | `Accept: application/json` | `503` JSON — app shows "starting..." |

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/routes/waker.rs` | Accept header check, return 503+JSON for API clients |
| `agent/src/routes/waker.rs` | Same change |

### Impact

- ~15 LoC across both files
- Zero risk (only changes response format, no logic changes)
- Also fixes SEO issue (search engines get 503, won't index loading page)

---

## Feature 2: Volume Persistence for Single-Service

### Problem

All container data is lost on redeploy, restart, or scale-to-zero wake. Databases, uploaded files, and configuration are wiped. This is the #1 blocker for using LiteBin with databases.

### Solution

Expand `projects/{project_id}/` (currently only holds `.env`) into a project data directory. Add bind mount support to `run_container()`.

> **Note:** This phase covers **bind mounts only** (data persisted to `projects/{id}/data/` on the host filesystem). Docker named volumes (NFS, tmpfs, custom drivers) are deferred to the MVP phase where the `project_volumes` table supports both types. Bind mounts cover 99% of use cases and are trivial to backup/migrate via `rsync` or `tar`.

```
projects/
├── myapp/
│   ├── .env              ← existing: shared env vars
│   └── data/             ← NEW: bind mount targets
│       └── app/          ← bind mount → container /app/uploads
```

### Deploy API Change

Add optional `volumes` field to the existing deploy request:

```json
{
  "project_id": "my-app",
  "image": "ghcr.io/me/myapp:latest",
  "port": 3000,
  "volumes": [
    { "path": "/app/uploads" },
    { "path": "/app/data", "name": "pgdata" }
  ],
  "cleanup_volumes": false
}
```

- `path` (required): path inside the container
- `name` (optional): directory name under `projects/{id}/data/`, defaults to project_id
- `cleanup_volumes` (optional, default `false`): remove data directories not in the new deploy's volumes list

### Docker Changes

Add `binds` to `HostConfig` in `run_container()`:

```rust
// In run_container(), before creating the container:
let mut binds: Vec<String> = Vec::new();
for vol in &project.volumes {
    let dir_name = vol.name.as_deref().unwrap_or(&project.id);
    let host_dir = ensure_data_dir(&projects_base, &project.id, dir_name)?;
    binds.push(format!("{}:{}", host_dir.display(), vol.path));
}
host_config.binds = Some(binds);
```

`ensure_data_dir()` is a simple `std::fs::create_dir_all()`:

```rust
fn ensure_data_dir(base: &str, project_id: &str, name: &str) -> std::io::Result<PathBuf> {
    let dir = PathBuf::from(base).join(project_id).join("data").join(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
```

### Volume Cleanup on Redeploy

When a user redeploys with changed volumes, old data directories may become orphaned:

- **Default: preserved** (no data loss on redeploy)
- **Opt-in: deleted** via `cleanup_volumes: true` in the deploy request
- Deploy response lists any orphaned volume paths so the user is aware

### Volume Cleanup Endpoint

Separate endpoint for explicit data cleanup (dashboard "Remove Data" button, CLI `l8b volume rm`):

```
DELETE /projects/:id/volumes/:name     — Remove a specific volume directory
DELETE /projects/:id/volumes            — Remove all volume directories
```

Dashboard and CLI prompt for confirmation before calling this (default is always keep data).

### CLI

```
l8b deploy --image ghcr.io/me/myapp:latest my-app
l8b deploy --cleanup-volumes --image ghcr.io/me/myapp:latest my-app
l8b volume rm my-app pgdata
l8b volume rm my-app --all
```

### Files Modified

| File | Change |
|---|---|
| `litebin-common/src/docker.rs` | `binds` on HostConfig, `ensure_data_dir()` |
| `litebin-common/src/types.rs` | `Volume` struct, `Project.volumes` field |
| `orchestrator/src/routes/deploy.rs` | Accept volumes + `cleanup_volumes` in deploy request, create dirs, orphan detection |
| `orchestrator/src/routes/volumes.rs` | New — DELETE endpoints for volume cleanup |
| `agent/src/routes/containers.rs` | Accept volumes in RunRequest, create dirs |
| Dashboard | "Remove Data" button with confirmation dialog |
| CLI | `l8b volume rm` subcommand, `--cleanup-volumes` deploy flag |

### Impact

- ~50-80 LoC total
- Zero risk for existing deploys (volumes field is optional, backward compatible)
- Permanently solves data persistence for single-service projects
- Data directory is ready for multi-service (just add more subdirectories)

---

## Feature 3: Custom Routing Rules

### Problem

Users can't define custom routing from the dashboard or CLI. If a frontend needs to proxy `/api/*` to an API service, they must configure it inside the container. This also prevents routing across projects (e.g., `myapp.l8b.in/api` → another project's API service).

### Solution

A CRUD layer for Caddy routing rules. Users add path-based or subdomain routes from the dashboard or CLI. Zero container restarts — just pushes a new Caddy config.

### Data Model

```sql
CREATE TABLE project_routes (
    id              INTEGER PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    route_type      TEXT NOT NULL DEFAULT 'path',  -- "path" or "subdomain"
    path            TEXT,          -- "/api/*" (for path-based routes)
    subdomain       TEXT,          -- "api" (for subdomain-based routes)
    upstream        TEXT NOT NULL,  -- "litebin-myapp-api:3001" (Docker network name + internal port)
    priority        INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT (unixepoch())
);
```

### Route Types

**Path-based** — matches a path pattern on all project hosts:

```
Route: /api/*
Matches: myapp.l8b.in/api/*, myapp.com/api/* (if custom domain exists)
```

**Subdomain-based** — user provides only the subdomain prefix, LiteBin expands to all project hosts:

```
Route: api (subdomain prefix)
Expands to: api.myapp.l8b.in, api.myapp.com (if custom domain exists)
DNS: auto-created in Cloudflare DNS mode (user adds manually in master proxy mode)
```

### API

```
GET    /projects/:id/routes          — List routes for a project
POST   /projects/:id/routes          — Add a route
DELETE /projects/:id/routes/:id      — Remove a route
```

### CLI

```
l8b route add myapp /api/* litebin-myapp-api:3001
l8b route add myapp --subdomain api litebin-myapp-api:3001
l8b route list myapp
l8b route remove myapp 3
```

### Dashboard

Project card shows a "Routes" section with a list of custom routes and an "Add Route" form with two input modes: path or subdomain.

### Caddy Integration

When resolving routes, include custom routes alongside the default project route. Custom routes are matched by priority (lower = first). The default project route (catch-all) always has the lowest priority.

Route expansion: path-based routes include all project hosts in the match. Subdomain-based routes expand the prefix to all project hosts.

```json
[
  { "match": [{"host": ["myapp.l8b.in", "myapp.com"], "path": ["/api/*"]}],
    "handle": [{"reverse_proxy": [{"upstreams": [{"dial": "litebin-myapp-api:3001"}]}]}] },
  { "match": [{"host": ["api.myapp.l8b.in", "api.myapp.com"]}],
    "handle": [{"reverse_proxy": [{"upstreams": [{"dial": "litebin-myapp-api:3001"}]}]}] },
  { "match": [{"host": ["myapp.l8b.in", "myapp.com"]}],
    "handle": [{"reverse_proxy": [{"upstreams": [{"dial": "litebin-myapp:3000"}]}]}] }
]
```

### Upstream Format

The upstream is always a Docker network name + internal port (not `localhost`, not a host-mapped port). This is how Caddy already resolves containers — it runs inside Docker on `litebin-network`.

```
# Correct — Docker DNS name + internal port
litebin-myapp-api:3001

# Works — public service of another project (on litebin-network)
litebin-otherapp:3000

# Doesn't work — internal service not on litebin-network
litebin-myapp-db:5432
```

### Files Modified

| File | Change |
|---|---|
| `orchestrator/src/db/migrations/` | New migration for `project_routes` |
| `orchestrator/src/routes/routes.rs` | New — CRUD endpoints |
| `orchestrator/src/routing_helpers.rs` | Include custom routes in `resolve_routes()` |
| `litebin-common/src/types.rs` | `ProjectRoute` struct |
| Dashboard | Route list + add form in project settings |

### Impact

- ~200-300 LoC (CRUD + Caddy integration + dashboard)
- Zero risk (additive, existing routes unchanged, custom routes are additional)
- Works for single-service and multi-service projects
- Enables cross-project routing (to public services of other projects)

---

## Implementation Order

All three features are independent and can ship in any order. Suggested order based on value:

```
1. Feature 1: Waker 503       (~15 LoC, immediate user value)
2. Feature 2: Volumes         (~80 LoC, biggest user pain point)
3. Feature 3: Custom Routes   (~300 LoC, new feature but standalone)
```

Total: ~400 LoC across all three features. All backward compatible. All benefit existing users before multi-service ships.

> **Note:** Missing Docker features (`entrypoint`, `shm_size`, `working_dir`, `user`, `tmpfs`, `read_only`, `extra_hosts`) are not included here. They come for free with the MVP's `run_service_container` + compose file support — no need to add them to the single-service `run_container()` separately.
