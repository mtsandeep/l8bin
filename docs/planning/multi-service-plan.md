# Multi-Service Implementation Plan

This plan has been split into three implementation stages for easier management:

## Stage 1: Pre-MVP — [pre-mvp-plan.md](pre-mvp-plan.md)

Standalone improvements that ship before multi-service. Every item adds value to existing single-service users today and is a prerequisite or enabler for multi-service.

| Feature | Description | LoC |
|---|---|---|
| Waker 503 + JSON | Return 503+JSON for API clients instead of HTML loading page | ~15 |
| Volume Persistence | Bind mounts under `projects/{id}/data/` survive container recreation | ~80 |
| Custom Routing Rules | CRUD for Caddy routes via dashboard/CLI | ~300 |

## Stage 2: MVP — [mvp-plan.md](mvp-plan.md)

Core multi-service implementation. After this, users can deploy multi-service projects via compose file with isolated per-project networks, volume persistence, dependency ordering, and scale-to-zero.

| Phase | Description |
|---|---|
| Phase 1 | Data model (`project_services`, `project_volumes`) + migration |
| Phase 2 | Docker: per-project networks + `run_service_container` |
| Phase 3 | Compose deploy (serde extraction, 4 validation checks, unified code path) |
| Phase 4 | Lifecycle: start/stop/delete/recreate for multi-service |
| Phase 5 | Routing: Caddy to public service |
| Phase 6 | Agent: waker + janitor multi-service support |

## Stage 3: Post-MVP — [post-mvp-plan.md](post-mvp-plan.md)

Incremental enhancements after MVP ships.

| Feature | Description |
|---|---|
| Health Checks | Readiness polling during deploy/wake for `depends_on` |
| Service Catalog | One-click database/cache add-ons (Postgres, Redis, etc.) |
| Dashboard View | Read-only service list with stats aggregation |
| CLI Support | Compose deploy, catalog, service management commands |
| Multi-Subdomain Routing | Different subdomains → different services |

---

## Key Components

- [compose-bollard](compose-bollard-crate.md) — Internal crate for compose YAML → bollard Docker API mapping

## Key Design Decisions

### Unified Code Path

Every project internally uses `project_services`. Single-service is just multi-service with one service. The deploy endpoint is the only branching point — it normalizes input, then one code path handles everything.

### Compose Handling

Serde struct deserialization (~140 lines), not a compose interpreter. Unknown fields silently ignored. Only 4 LiteBin-logic validation checks (cycles, ghost deps, public service detection, multiple public services). Everything else — let Docker validate.

### Security Model

Architectural isolation only. Per-project Docker networks + Caddy routing + no host port mapping. No compose validation for security (self-hosted — user's server, user's code).

### Volume Persistence

Bind mounts to `projects/{id}/data/` over Docker named volumes. Enables trivial backup/restore/clone/migrate via filesystem operations.

