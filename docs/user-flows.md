# User & Service Flows

Every scenario the system must handle — user-triggered actions, automatic behaviors, and edge cases. This is the definitive checklist for what LiteBin covers.

---

## Deploy Flows

### D1. Deploy single-service project (local)
User pushes code via CLI. Orchestrator creates container, starts it, syncs Caddy route.

### D2. Deploy single-service project (remote agent)
User pushes code, orchestrator delegates to agent. Agent creates+starts container, reports back.

### D3. Deploy multi-service project (local)
User pushes compose.yaml. Orchestrator creates per-project network, starts services level-by-level respecting dependencies, connects Caddy+orchestrator to network, syncs routes.

### D4. Deploy multi-service project (remote agent)
User pushes compose.yaml. Orchestrator delegates to agent's batch-run endpoint. Agent creates network, starts services, reports container info back.

### D5. Redeploy an already-running project
User pushes updated code/image. Old containers replaced with new ones cleanly. Deploy always recreates.

---

## Dashboard Flows

### S1. Start a stopped single-service project
User clicks Start. Container starts via fast path (`docker start`, not recreate). Site becomes accessible.

### S2. Start a stopped multi-service project
User clicks Start. All service containers start via fast path (`docker start` each). Falls back to recreate if a container was manually deleted.

### S3. Stop a running single-service project
User clicks Stop. Container stops. Site shows offline page. Container preserved for fast restart.

### S4. Stop a running multi-service project
User clicks Stop. All service containers stop. Site shows offline page. Containers preserved.

### S5. Stop an individual service in a multi-service project
User clicks Stop on one service (e.g., backend). Other services keep running. Public service stays accessible. Next visit detects the down service and recovers it silently in background.

### S6. Start an individual service in a multi-service project
User clicks Start on a stopped service. Only that service starts (recreates — stops+removes old container first). Others unaffected.

### S7. Restart an individual service
User clicks Restart. Old container removed, new one created. Brief downtime for that service only.

### S8. Recreate a single-service project
User clicks Recreate. Old container removed, new one created from current compose/env. Brief downtime.

### S9. Recreate a multi-service project (all services)
User clicks Recreate. Modal appears listing all services with checkboxes (all checked by default). User confirms. All checked containers removed and recreated. Unchecked services untouched.

### S10. Recreate selected services in a multi-service project
User clicks Recreate. Modal appears. User unchecks some services, confirms. Only checked services are recreated. Unchecked services keep running untouched.

### S11. Redeploy a multi-service project
User clicks Redeploy. Same service selection modal as Recreate. Selected services have their images pulled and containers recreated. Unchecked services untouched.

### S12. Delete a project
User clicks Delete. All containers removed, project network removed, DB rows deleted, Caddy routes removed.

---

## Wake / Auto-Start Flows

### W1. Visit URL of a stopped project (auto-start enabled)
Loading page shown. Container started in background. Next poll serves the site. Works for both single and multi-service (local + remote).

### W2. Visit URL of a stopped project (auto-start disabled)
Offline page shown. No auto-start.

### W3. Visit URL of a running single-service project
Direct proxy to container. No loading page.

### W4. Visit URL of a running multi-service project
Health check all services (throttled 5s). If all healthy, proxy to public service. If non-public service crashed, silently recover in background while serving the public service.

### W5. Visit URL when public service crashed (multi-service)
Loading page shown. All containers restarted in background. Next poll serves the site.

### W6. Visit URL when non-public service crashed (multi-service)
No loading page. Public service proxied immediately. Crashed service recovered silently in background.

### W7. Visit URL during an ongoing wake (concurrent requests)
All concurrent requests get the loading page. Only one background wake runs (single-flight lock).

### W8. Visit URL after wake completes but DNS not ready
Still shows loading page (wake lock check), not 502.

---

## Manual Container Operations (Edge Cases)

### E1. User manually `docker stop` a service container
Next visit detects it's down and recovers (same as W5/W6 depending on which service).

### E2. User manually `docker rm` a service container
Container gone but DB still has `container_id`. Next operation detects the mismatch — `docker start` fails on non-existent container, falls back to recreate.

### E3. User manually `docker rm` the public service container
Same as E2. The public service is just another service going through the same `start_services` path.

### E4. User manually deletes the per-project Docker network
Next start/wake recreates the network before starting containers (`ensure_project_network` is idempotent).

### E5. User manually starts a stopped container
Harmless — next health check sees it running, no action needed.

### E6. Orchestrator restarts while containers are running
Orchestrator reconnects to all project networks on startup. Next visit proxies immediately.

### E7. Agent restarts while containers are running
Agent reconnects to all project networks on startup. Caddy config restored from persisted file. Next visit proxies immediately.

### E8. Orchestrator restarts while containers are stopped
Reconciliation checks DB vs reality on startup. Containers still stopped, DB says stopped. No action until user visits URL or clicks Start.

---

## Auto-Sleep Flows

### A1. Project auto-sleeps after idle timeout
Janitor detects idle project (no traffic for N minutes). Stops all containers. Preserves them for fast restart. Removes Caddy route.

### A2. Auto-sleep a multi-service project
Same as A1 but stops all service containers. Preserves all containers.

### A3. Visit URL of auto-slept project
Same as W1 — loading page, auto-start, proxy.

---

## Remote Agent Flows

### R1. Deploy to remote agent, visit URL
**master_proxy mode**: Request hits orchestrator → proxies to agent. Agent wake server handles container lifecycle.
**cloudflare_dns mode**: DNS resolves directly to agent's public IP. Agent Caddy handles request independently.

### R2. Remote agent container crashes
Agent detects on next visit (health check), recovers in background. No orchestrator needed for the actual wake. Public service down → wake lock + restart.

### R3. Agent goes offline
**master_proxy mode**: Orchestrator heartbeat detects offline status. Requests return error/offline page.
**cloudflare_dns mode**: DNS still points to agent. Requests fail at TCP level. Dashboard shows agent offline.

### R4. Agent comes back online
Heartbeat resumes. Reconciliation runs. Project status corrected if needed.

---

## Cloudflare DNS Mode — Agent Independence

In `cloudflare_dns` routing mode, DNS A records point directly to agent nodes. The agent operates independently for all traffic-serving and container lifecycle operations. The orchestrator is only needed for dashboard, new deploys, and DNS management.

### CF1. Agent serves traffic while orchestrator is down
Agent Caddy routes directly to containers. Wake, health checks, proxy all work locally. No orchestrator needed in the data path.

### CF2. Agent wakes a stopped container while orchestrator is down
Agent reads compose.yaml/.env from local disk, starts containers via Docker, rebuilds local Caddy config. Wake report to orchestrator silently fails (fire-and-forget).

### CF3. Agent auto-recovers crashed service while orchestrator is down
Health check detects down service, spawns background wake. Recovers silently. Orchestrator DB becomes stale but self-heals when orchestrator comes back.

### CF4. Agent restarts while orchestrator is down
Agent loads persisted Caddy config from `data/caddy-config.json`, reconnects to project networks. All previously running containers become reachable again.

### CF5. Orchestrator comes back after being down
Heartbeat from agent resumes. Wake reports update DB. Reconciliation corrects stale statuses. DNS and route sync catch up.

### CF6. Selective recreate/redeploy while orchestrator is down
**NOT APPLICABLE.** Dashboard actions go through the orchestrator API. If orchestrator is down, the dashboard is also down. The agent only handles wake (traffic-triggered) and caddy-sync (orchestrator push) operations.

---

## Key Principle: Agent Independence

> In `cloudflare_dns` mode, the agent is the **primary data path**. The orchestrator is a **control plane** for management, not a proxy.
>
> All new features must keep this in mind:
> - Dashboard-initiated actions (recreate, redeploy, start, stop) go through orchestrator → agent. These require the orchestrator to be up.
> - Traffic-triggered actions (wake, health check, proxy) are handled entirely by the agent. These work even when the orchestrator is down.
> - DB updates from the agent are fire-and-forget (wake reports, heartbeats). Losing them is acceptable — they self-heal on next report.
> - The agent stores everything it needs locally: compose.yaml, .env, metadata.json, Caddy config, registration state.

---

## Static Assets / Sub-resources

### F1. CSS/JS/image requests while project is starting
Non-HTML requests get a JSON 502 response (not the HTML loading page), so the browser doesn't replace the loading spinner with an error page.

### F2. CSS/JS/image requests while project is running
Proxied normally, no loading page.

### F3. Favicon/browser extension requests (non-project paths)
Should not trigger a wake. Returns 404.

---

## Concurrency Flows

All container-modifying operations share a single `project_locks` per project, serialized via Semaphore. No race conditions between wake, deploy, stop, recreate, or janitor.

### C1. User clicks Start while auto-wake is in progress
Lock serializes them. User gets loading page.

### C2. User clicks Stop while auto-wake is in progress
Lock serializes them. Stop waits for wake to finish, or runs first and wake handles gracefully.

### C3. User deploys while containers are running
Lock serializes. Deploy waits, then recreates.

### C4. User deploys while auto-wake is in progress
Lock serializes. No concurrent start+deploy on the same project.

### C5. Janitor tries to sleep project while user is actively browsing
Janitor checks `last_active_at`. Activity tracker updates this on each request. Active projects are not slept.

---

## Further Reading

- [Multi-Service Architecture](multi-service.md) — per-project networks, dependency levels, health checks, unified start_services
- [Waker](waker.md) — detailed wake-on-request flow diagrams
- [Architecture](architecture.md) — full system overview, component responsibilities
- [Failure Model](failure-model.md) — how every component handles failures and recovery
