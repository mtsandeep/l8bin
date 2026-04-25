# Multi-Service Post-MVP Plan

Incremental enhancements after the core multi-service MVP ships. These add polish and convenience but are not required for multi-service to work.

**Prerequisite:** Multi-service MVP (v0.2) — completed

---

## Feature 1: Dependency Health Checks

### What

Docker healthcheck config for services with dependents. Readiness polling during deploy and wake — start dependent services only after dependencies are healthy.

### Three Levels of Dependency Resolution

| Level | Mechanism | Status |
|---|---|---|
| DNS resolution | Docker bridge network | MVP (Phase 2) |
| Startup ordering | `depends_on` topological sort | MVP (Phase 3) — no readiness wait |
| Readiness waiting | Health checks + polling | **This feature** |

### Implementation

Health checks defined per service in compose map to Docker's `HealthConfig`:

```rust
HealthConfig {
    test: Some(vec!["CMD-SHELL".to_string(), "pg_isready -U app".to_string()]),
    interval: Some(3_000_000_000),
    timeout: Some(3_000_000_000),
    retries: Some(10),
}
```

**Readiness polling:** Before starting a dependent service, poll the dependency's health status until `healthy` (or fail on `unhealthy` / timeout).

### RAM Overhead

Docker's healthcheck executor uses ~1 MB per container. For 30 containers: ~30 MB.

**Recommendation:** Opt-in only. Enable `HealthConfig` only when a service has `depends_on` referencing it. Standalone services (no dependents) skip healthchecks — saves RAM on small VPS.

### Files Modified

| File | Change |
|---|---|
| `litebin-common/src/docker.rs` | HealthConfig mapping, `wait_for_healthy()` polling |
| `orchestrator/src/routes/deploy.rs` | Health check wait in deploy loop |
| `orchestrator/src/routes/waker.rs` | Health check wait in wake loop |
| `project_services` table | Already has `healthcheck` column (added in MVP migration) |

### Impact

- Deploy/wake time: +3-30s per dependency with healthcheck
- Runtime RAM: ~1 MB per container with healthcheck (Docker overhead)

---

## Feature 2: Template Catalog

See [template-catalog.md](template-catalog.md) for full design.

One-click project deployment from templates. Users pick a template (e-commerce, blog, standalone database), fill in prompted values (name, email, auto-generated passwords), and deploy. Templates include compose files, a LiteBin manifest (prompts, routes, public service), and env defaults.

---

## Feature 3: Dashboard Multi-Service View

### What

Read-only service view in the dashboard. Users who need multi-service already know docker-compose — no add/remove/edit forms.

### Design Principle

Separate static config from live state. Service config changes only on deploy, not every 5s.

| Data | Changes | Strategy |
|---|---|---|
| Project (status, name) | Every poll | Poll every 5s (existing) |
| Service config (image, port, deps) | Only on deploy | Load once on project open, cache |
| Stats (CPU, RAM, disk) | Every poll | Poll every 5s, sum across services |

### UI Changes

- **Project card:** Show `3 services` badge. Click to expand service list.
- **Service list (read-only):**
  - Service name, image, port, status, public/internal badge
  - Dependency chain: `web → db → redis`
  - Data directory size per service
  - Data/compose/env paths for SSH access
- **Stats aggregation:** Sum CPU/RAM/disk across all service containers:
  ```
  myapp: CPU 2.1% (web 1.5% + db 0.4% + redis 0.2%), MEM 284 MB, DISK 1.8 GB
  ```

### API

```
GET /projects/:id/services    — List services with config (cached until next deploy)
```

### Files

| File | Change |
|---|---|
| `orchestrator/src/routes/services.rs` | New — service list endpoint |
| `dashboard/src/components/ProjectCard.tsx` | Service count badge, expandable service list |
| `dashboard/src/components/ServiceList.tsx` | New — read-only service details |
| `dashboard/src/api.ts` | Service API calls |

### Impact

- ~150 LoC total (badge, service list, dependency graph, data paths)

---

## Feature 4: CLI Multi-Service Support

### What

CLI commands for compose deploy and service management.

### Commands

```
l8b deploy --compose docker-compose.yml my-app
l8b catalog list
l8b catalog add my-app postgres
l8b service list my-app
```

### Files

| File | Change |
|---|---|
| `cli/src/main.rs` | Compose deploy flag, catalog subcommands, service subcommands |

---

## Feature 5: Multi-Subdomain Routing

### What

Route different subdomains to different services within the same project. E.g., `api.myapp.l8b.in` → API service, `myapp.l8b.in` → web service.

This is an advanced use case. Most multi-service projects route through a single public service (e.g., a frontend that proxies to the API). This feature enables direct access to individual services via subdomain.

### Design

- User marks services as `litebin.public: "true"` with an optional subdomain label: `litebin.subdomain: "api"`
- Caddy gets one route per public service: `api.myapp.l8b.in` → `litebin-myapp-api:3001`
- Falls back to default project domain for services without a subdomain label

### Files

| File | Change |
|---|---|
| `orchestrator/src/routing_helpers.rs` | Multiple public service routes per project |
| `orchestrator/src/routes/waker.rs` | Wake on any public service subdomain |

---

## Future Considerations

| Item | Notes |
|---|---|
| Compose `build:` support | Requires build agent. CI/CD build is preferred for now. |
| Variable interpolation | `.env` file only. Shell env passthrough unsupported. |
| Large volume migrations | Stream via orchestrator. Roadmap Phase 2 (App Migration). |
| Multi-service + previews | Preview system must create per-preview networks. |
| Health check timeout policy | Fail deploy is safest. Consider configurable policy later. |
| Per-project resource quotas | Currently per-service limits only. No total limit across services. |
| Node migration | Stop all → rsync `projects/{id}/data/` → start all on new node. Roadmap Phase 2. |

---

## Implementation Order

Suggested priority based on user impact:

```
1. Feature 3: Dashboard View    (~150 LoC, high visibility, users can see their services)
2. Feature 1: Health Checks      (~100 LoC, prevents startup race conditions)
3. Feature 4: CLI Support        (~80 LoC, power users need it)
4. Feature 2: Catalog            (~200 LoC, convenience for common add-ons)
5. Feature 5: Multi-Subdomain    (~100 LoC, advanced use case, low demand)
```
