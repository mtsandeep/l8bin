# Roadmap

Planned features beyond the current release. Ordered by priority and dependency.

---

## What We Have Today

### Deployment
- Single-service Docker container deployment
- Multi-service docker-compose deployment
- GitHub Action for CI/CD auto-deploy
- CLI tool (`l8b`) for terminal workflow
- Custom Docker images (including `sha256:` uploaded images)
- Per-project environment variables (`.env` on agent filesystem)
- Bind mount data support

### Networking
- Automatic TLS via Caddy (on-demand Let's Encrypt)
- Custom domains, custom routes (path + subdomain)
- Cloudflare DNS integration (automatic A record management)
- Master proxy mode or direct DNS mode
- Auto-wake on request, auto-sleep after inactivity

### Multi-Node
- Orchestrator + agent architecture with mTLS
- Add/remove agent nodes from dashboard
- Deploy projects to any node
- Agent health monitoring and reconciliation
- Auto-start on agent reconnect

### Management
- Web dashboard
- API tokens for programmatic access
- Resource limits (CPU/memory) per project
- Docker socket proxy (controlled Docker access per project)
- Project flags (auto-start, raw ports, Docker access)
- Janitor (auto-stop idle projects)
- Crash recovery via auto-wake

### Observability
- Metrics (CPU/memory/disk per project)
- Basic log streaming per service (10MB stored)

---

## Phase 1: Preview Environments

Spin up an isolated container for every pull request or branch, with its own subdomain. Auto-cleanup when the PR closes.

### Why This First

- Highest-impact feature for adoption — no self-hosted PaaS at this price point does this
- Leverages existing architecture (Caddy on-demand TLS, scale-to-zero, deploy pipeline)
- Creates natural multi-container usage (production + N previews)
- Aligns with "for side projects and demos" positioning

### User Experience

```
1. Link a GitHub repo to a LiteBin project
2. Open a PR → container spins up at pr-42.myapp.l8b.in
3. Push updates to the PR → container rebuilds automatically
4. Comment appears on the PR with the preview URL
5. PR merges/closes → container removed after configurable delay
```

### Naming Convention

```
pr-{number}.{project}.{domain}     → PR-based (default)
{branch}.{project}.{domain}        → branch-based (optional, for non-PR workflows)
```

PR-based is the default because branch names can contain invalid DNS characters (`feature/`, `_`, etc.).

### Webhook Flow

```
GitHub PR opened/pushed
       │
       ▼
POST /webhooks/github
  - Verify HMAC-SHA256 signature
  - Parse event: pull_request (opened, synchronize, closed)
       │
       ▼
opened / synchronize:
  1. Resolve project by repo URL
  2. Build image (GitHub Actions pushes to registry, or pre-built image)
  3. Create container: litebin-{project}-pr-{number}
  4. Route: pr-{number}.{project}.{domain}
  5. Post PR comment with preview URL
  6. Set auto-stop (shorter timeout, e.g. 30min)
       │
       ▼
closed / merged:
  1. Find preview container for this PR
  2. Remove container + Caddy route
  3. Delete PR comment (optional)
```

### Build Strategy

Builds happen outside the VPS to avoid CPU/RAM load:

| Method | How | Existing? |
|--------|-----|-----------|
| GitHub Actions | CI builds image, pushes to registry, triggers LiteBin deploy | Yes — `litebin-action` already does this |
| GitLab CI | Same pattern, different CI | No — but same deploy API works |
| Bring your own image | User builds locally with `l8b deploy`, preview just manages lifecycle | Yes — `POST /deploy` works today |
| Future: external builders | Dedicated build service (separate from VPS) | Future (template catalog add-on) |

**Recommended approach:** The webhook receiver does NOT build images. It expects the CI pipeline to have already pushed an image tagged with the PR number. The webhook triggers a deploy of that pre-built image.

```yaml
# GitHub Actions workflow (.github/workflows/preview.yml)
on:
  pull_request:
    types: [opened, synchronize, closed]

jobs:
  build-and-deploy:
    if: github.event.action != 'closed'
    steps:
      - uses: actions/checkout@v4
      - run: docker build -t ghcr.io/me/myapp:pr-${{ github.event.number }} .
      - run: docker push ghcr.io/me/myapp:pr-${{ github.event.number }}
      - uses: mtsandeep/l8bin-action@v1
        with:
          server: ${{ secrets.L8B_SERVER }}
          token: ${{ secrets.L8B_TOKEN }}
          project_id: "myapp-pr-${{ github.event.number }}"
          image: "ghcr.io/me/myapp:pr-${{ github.event.number }}"

  cleanup:
    if: github.event.action == 'closed'
    steps:
      - run: curl -X DELETE ${{ secrets.L8B_SERVER }}/projects/myapp-pr-${{ github.event.number }}
```

### Data Model

New `previews` table:

```sql
CREATE TABLE previews (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    preview_id      TEXT NOT NULL,           -- "pr-42"
    subdomain       TEXT NOT NULL,           -- "pr-42.myapp.l8b.in"
    provider        TEXT NOT NULL,           -- "github"
    repo_url        TEXT NOT NULL,           -- "https://github.com/org/repo"
    pr_number       INTEGER,                 -- 42
    branch          TEXT,                    -- "feat/login"
    commit_sha      TEXT,
    status          TEXT NOT NULL DEFAULT 'pending',  -- pending, running, stopped, error
    container_id    TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(project_id, preview_id)
);
```

Per-project GitHub config:

```sql
CREATE TABLE project_github_config (
    project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    repo_url        TEXT NOT NULL,
    webhook_secret  TEXT NOT NULL,           -- HMAC-SHA256 secret
    auto_stop_mins  INTEGER DEFAULT 30,      -- shorter than production
    cleanup_delay_mins INTEGER DEFAULT 60,   -- delay before deleting closed PR containers
    branch_pattern  TEXT DEFAULT 'pr',       -- "pr" or "branch"
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### API Endpoints

```
POST   /webhooks/github                    -- GitHub webhook receiver
GET    /projects/:id/previews              -- List active previews for a project
DELETE /projects/:id/previews/:preview_id  -- Manually delete a preview
POST   /projects/:id/github/config         -- Set up GitHub integration
DELETE /projects/:id/github/config         -- Remove GitHub integration
GET    /projects/:id/github/config         -- Get GitHub config
```

### Dashboard Changes

- **Project card:** Show active preview count badge
- **Preview list:** Expandable section under each project showing active previews with links, status, PR number, commit SHA
- **GitHub config modal:** Enter repo URL, auto-generate webhook secret, display webhook URL to configure in GitHub
- **Preview actions:** Stop, restart, delete individual previews

### CLI Changes

```
l8b preview list <project>          -- List active previews
l8b preview delete <project> <id>   -- Delete a preview
```

### Edge Cases

| Scenario | Handling |
|----------|----------|
| PR force-pushed | Treated as `synchronize` — rebuild with new commit |
| PR reopened | Treated as `opened` — new container if old one was cleaned up |
| Multiple PRs for same project | Each gets its own container and subdomain |
| Container limit reached | Reject new preview, comment on PR with error |
| Agent down during PR open | Queue preview, deploy when agent is back (or fail with PR comment) |
| Preview container crashes | Auto-restart policy (same as production containers) |
| Race: webhook before CI finishes pushing image | Webhook should wait or accept deferred deploy |

### Files to Modify

| File | Change |
|------|--------|
| `orchestrator/src/main.rs` | Add webhook + preview routes |
| `orchestrator/src/routes/webhooks.rs` | New — GitHub webhook handler |
| `orchestrator/src/routes/previews.rs` | New — Preview CRUD |
| `orchestrator/src/routes/projects.rs` | GitHub config endpoints |
| `litebin-common/src/types.rs` | Preview, GitHubConfig types |
| `orchestrator/src/db/migrations/` | New migration for `previews` + `project_github_config` |
| `orchestrator/src/routing_helpers.rs` | Include preview subdomains in route resolution |
| `orchestrator/src/routes/waker.rs` | Handle preview subdomain wake |
| `agent/src/routes/waker.rs` | Handle preview subdomain wake (cloudflare DNS mode) |
| `dashboard/src/` | Preview list, GitHub config modal |
| `cli/src/main.rs` | Preview subcommands |

---

## Phase 2: App Migration

Move projects between nodes with configurable options. Makes multi-node useful for rebalancing and server upgrades. Full plan: [migration.md](migration.md).

### User Experience

```
1. Select project → "Migrate to node" → pick target
2. Choose what to move: config (always), image (optional), volumes (optional)
3. Place .env on target (user handles secrets)
4. Source keeps running (dual-run), target deploys
5. Optional: enable maintenance mode on source
6. Verify target works → trigger cleanup on source when ready
```

### Key Design Decisions

- **Dual-run safety** — source stays running until user explicitly triggers cleanup
- **`migrated` flag** — DB tracks migrated-but-not-cleaned projects, dashboard shows indicator
- **On-demand cleanup** — `POST /projects/:id/migrate/cleanup` is never automatic
- **Architecture warning** — soft warning for cross-arch, not a hard block
- **`.env*` never transferred** — user places manually on target
- **Maintenance mode** — standalone feature, serves 503 page with admin cookie bypass
- **Chunked volume transfer** — 100MB chunks, resumable on failure
- **Project duplication** — clone to same or different node (shares migration infrastructure)

### Endpoints

```
POST /projects/:id/migrate              -- Start migration
POST /projects/:id/migrate/cleanup      -- Clean up source (on-demand)
POST /projects/:id/maintenance          -- Toggle maintenance mode
POST /projects/:id/duplicate            -- Clone project
```

### Master Migration + Promote

Migrate orchestrator data to an agent, promote it to master:

```
1. Migrate all projects off master (to agents)
2. Transfer DB, config, project files to target agent
3. SSH to agent → ./install.sh promote
4. Agent becomes new master, old master decommissioned manually
```

---

## Phase 3: Platform Backup (SQLite)

SQLite backup via Litestream sidecar. No custom backup code in LiteBin. Full plan: [backup.md](backup.md).

### User Experience

```
1. Install LiteBin → Litestream starts automatically (local file backup)
2. Optional: add S3/R2 from dashboard for cloud backups
3. Restore: l8b backup restore (from local or S3, point-in-time)
```

### Design

- **Litestream as optional sidecar** — always-on for local, S3 configurable later
- **Zero custom code** — LiteBin generates config, manages container, that's it
- **~5-10 MB RAM** extra
- **Real-time WAL streaming**, point-in-time recovery, LZ4 compression
- **Local backup by default** during `install.sh`, S3/R2 post-install

### Configuration Points

- **install.sh** — always includes Litestream with local file replica
- **Dashboard** — add/remove S3 replicas, view status, trigger restore
- **CLI** — `l8b backup status`, `l8b backup add-s3`, `l8b backup restore`

---

## Phase 4: One-Click Apps (Template Catalog)

Template catalog of popular apps deployable with a single click. Full plan: [template-catalog.md](template-catalog.md).

### User Experience

```
1. Dashboard → "New Project" → "From Template"
2. Browse catalog (WordPress, Next.js, n8n, Mastodon, etc.)
3. Click "Deploy" → configure domain + env vars
4. Running
```

### Approach

- Templates are docker-compose files with a LiteBin manifest (template.yml)
- Initial support: pre-built image templates only (PostgreSQL, Redis, WordPress, etc.)
- Build server support for Dockerfile-based templates is a future add-on
- Official templates maintained by LiteBin, remote catalog served from URL
- Deploy uses existing compose deployment pipeline — no special logic needed

---

## Phase 5: Deploy History & Rollback

Keep previous deploys and allow instant rollback.

### User Experience

```
l8b history myapp
# → #3  2026-05-01 10:30  ghcr.io/me/myapp:abc123
# → #2  2026-04-30 15:00  ghcr.io/me/myapp:def456  ← rollback to this
# → #1  2026-04-29 09:00  ghcr.io/me/myapp:789abc

l8b rollback myapp 2
# → Switches to previous image, keeps volumes intact
```

### Open Questions

- Store previous image tags only (lightweight) or full image exports (heavy)?
- How many deploys to keep? Configurable per project?
- Should rollback also revert .env changes?

---

## Phase 6: Eject (Litebin Eject)

Export a project as a standalone Docker Compose + Caddy setup. The user "graduates" from LiteBin and takes their app with them — no LiteBin dependency needed on the target server.

### User Experience

```
l8b eject myapp
# → Generates a standalone setup:
#    ├── docker-compose.yml  (app + caddy)
#    ├── Caddyfile           (TLS + routing)
#    ├── .env                (copied from agent)
#    └── data/               (exported volumes)
#
# → Deploy anywhere: docker compose up -d
```

### Use Cases

- **Client handover** — give a running project to a client without LiteBin access
- **Server move** — move a project to a server where LiteBin isn't installed
- **Graduation** — project outgrew LiteBin, needs its own dedicated setup
- **Backup export** — standalone setup as a form of portable backup

### Approach

1. Export container image (`docker save`) or reference registry image
2. Export Docker volumes (reuse chunked export from migration plan)
3. Copy `.env` from agent (local operation, not over network)
4. Generate `docker-compose.yml` with app config (ports, volumes, restart policy)
5. Generate minimal `Caddyfile` (domain, TLS, reverse proxy)
6. Package into a tar — user extracts and runs `docker compose up -d`

### What's Included vs Excluded

| Included | Excluded |
|----------|----------|
| docker-compose.yml | LiteBin orchestrator/agent |
| Caddyfile | LiteBin dashboard |
| Container image | LiteBin mTLS certs |
| Volumes | LiteBin DB |
| .env | Auto-wake/sleep |
| Caddy TLS certs (if exported) | Custom routes |
| | Metrics/logs |

### CLI

```
l8b eject myapp                          # Export to ./ejected/myapp/
l8b eject myapp --output /path/to/dir   # Custom output directory
l8b eject myapp --include-image          # Bundle image (large, ~200MB+)
```

---

## Quick Wins (Can Be Done Anytime)

### Full Real-Time Log Streaming

Currently logs are stored up to 10MB per service. Full streaming via WebSocket for real-time tail.

- Dashboard: live log viewer with auto-scroll
- CLI: `l8b logs -f <project>` (follow mode)
- API: WebSocket endpoint for log streaming
- Optional: log aggregation (send to external Loki/ELK)

### Environment Variable Dashboard Editor

Per-project env var management via dashboard. Currently env vars are set at deploy time only.

- Dashboard: env var editor in project settings (read/write `.env*` on agent)
- CLI: `l8b env set myapp KEY=value`
- No redeploy needed for env changes (container restart only)

> **Note:** `.env*` files are never transferred over the network (core LiteBin principle). The dashboard editor writes directly to the agent's filesystem via API.

### Notifications

Event-driven notifications pushed to an external notification router. LiteBin writes to a local outbox and POSTs JSON — no provider code, no channel config. Full plan: [notifications.md](notifications.md).

- Global notification config (endpoint URL, dedupe window, severity filter)
- Project tags for router-side filtering (prod, staging, etc.)
- Events: deploy success/failure, crash loop, agent offline, backup failure, auto-update available
- Dashboard: config form, test button, notification log

---

## Phase 7: Zero-Downtime Deploys

Deploy a new version without stopping the current one. Start new container, verify it's healthy, switch traffic, then stop the old one.

### Why This Matters

Current deploy flow: stop container → pull image → start container. Every deploy has a gap where no container is serving requests. For production sites, even 10-30 seconds of downtime per deploy is painful — and if the new container crashes on start, users see errors until manual intervention.

### Approach

For single-service projects (the majority):

```
1. Pull new image
2. Start new container on a different port (or with a temp name)
3. Health check: HTTP GET /health (or TCP port check) — wait for healthy
4. Update Caddy reverse_proxy to point to new container
5. Old container continues serving in-flight requests (Caddy drains connections)
6. Stop old container
```

For multi-service compose projects: rolling update per service, respecting dependency order. Start updated service → health check → update dependent services one by one.

### Health Check During Deploy

Leverages the dependency health check system from post-MVP Feature 1. During deploy, the orchestrator polls the new container's health endpoint before switching traffic:

```rust
// Wait for new container to be healthy
for attempt in 0..max_retries {
    match container.health().await {
        HealthStatus::Healthy => break,      // switch traffic
        HealthStatus::Unhealthy => rollback, // stop new, keep old
        HealthStatus::Starting => sleep(interval).await,
    }
}
```

### Rollback on Failure

If the new container fails health checks within the timeout:

```
1. Stop new container (unhealthy)
2. Keep old container running (never stopped it)
3. Caddy still pointing to old container
4. No traffic disruption
5. Log deploy failure + notify (Quick Win: Notifications)
```

This is simpler and more reliable than the Phase 5 rollback (which redeploys a previous image). Zero-downtime deploy with automatic rollback on failure covers most production needs.

### Configuration

Per-project setting (default: off, opt-in):

```
zero_downtime: true
health_check:
  type: http          # http, tcp, none
  path: /health       # for http type
  port: 3000          # for tcp type (defaults to service port)
  timeout: 30s        # max wait for healthy
  interval: 3s        # time between checks
  failures: 3         # unhealthy after N consecutive failures
```

For single-service projects, health check config can be inferred from Docker's `HEALTHCHECK` instruction in the image (already supported by Docker API). Explicit config in LiteBin overrides the image's healthcheck.

### What Changes

| Component | Change |
|-----------|--------|
| `orchestrator/src/routes/deploy.rs` | Start new before stopping old, health check wait, Caddy update |
| `orchestrator/src/routes/waker.rs` | Same pattern for wake (if zero_downtime enabled) |
| `orchestrator/src/routing.rs` | Update Caddy upstream to new container |
| `projects` table | Add `zero_downtime` boolean column |
| `project_services` table | Add health check columns (type, path, port, timeout, interval, failures) — may already exist from post-MVP Feature 1 |

### Edge Cases

| Scenario | Handling |
|----------|----------|
| New image doesn't exist | Fail before starting new container, old keeps running |
| New container starts but health check fails | Automatic rollback, old container untouched |
| Port conflict (new container can't bind) | Fail, old keeps running |
| Multi-service with dependencies | Update services bottom-up (db first, then workers, then web) |
| Disk full (can't pull image) | Fail before affecting running container |
| `zero_downtime: false` (default) | Current behavior: stop → pull → start |

---

## Phase 8: Liveness Probes

Continuous health monitoring for running containers. Detect when an app is running but unhealthy (returning 500s, deadlocked, connection pool exhausted) and restart it automatically.

### Why This Matters

The post-MVP health checks (Feature 1) are **startup-time only** — they wait for dependencies to become healthy before starting dependent services. Once the app is running, nobody checks if it's still healthy.

Docker's `unless-stopped` restart policy only triggers on **process exit**. If the app process is alive but the app inside is broken (deadlock, OOM, connection pool exhaustion), Docker won't restart it. The container appears "running" but serves errors.

### Approach

The orchestrator periodically checks each running container's health and takes action:

```
Every 60 seconds (configurable):
  for each running project with liveness probe enabled:
    check health
    if healthy:
      increment consecutive_success
      if was degraded → trigger `unhealthy_recovered` notification ([notifications.md](notifications.md))
    if unhealthy:
      increment consecutive_failures
      if consecutive_failures >= threshold:
        restart container
        notify (via [notifications.md](notifications.md))
```

### Health Check Methods

| Method | How | Use Case |
|--------|-----|----------|
| `http` | GET request to `http://container:port/path`, 2xx = healthy | Web apps, APIs |
| `tcp` | TCP connection to `container:port`, connects = healthy | Database, Redis, any TCP service |
| `docker` | Use Docker's built-in health check (`HEALTHCHECK` in Dockerfile) | Apps with built-in health checks |
| `none` | Skip liveness probe | Default, opt-in only |

### Restart Behavior

On liveness failure (consecutive failures >= threshold):

1. **Stop the unhealthy container** (Docker stop, graceful shutdown)
2. **Wait 5 seconds** (prevent crash loops)
3. **Start the container** (same image, same config)
4. **Reset failure counter**

If the container fails liveness again immediately (crash loop):

```
Attempt 1: restart
Attempt 2: restart
Attempt 3: restart
Attempt 4+: stop, mark status "unhealthy", trigger `crash_loop` notification ([notifications.md](notifications.md)), do NOT auto-restart
         → user investigates manually
```

This prevents infinite restart loops from burning CPU and spamming notifications. The project is marked as `unhealthy` in the dashboard — distinct from `stopped` or `error`.

### Configuration

Per-project or per-service:

```
liveness_probe:
  enabled: true
  type: http              # http, tcp, docker, none
  path: /health           # for http
  port: 3000              # defaults to service port
  interval: 60s           # time between checks
  timeout: 5s             # per-check timeout
  failures: 3             # restart after N consecutive failures
  max_restarts: 3         # stop auto-restarting after N attempts (crash loop protection)
```

### Relationship to Other Features

| Feature | When It Runs | Purpose |
|---------|-------------|---------|
| Startup health check (post-MVP Feature 1) | During deploy/wake | Wait for dependencies before starting dependents |
| Deploy health check (Phase 7) | During deploy | Verify new container is healthy before switching traffic |
| **Liveness probe (this phase)** | **Continuously, while running** | **Detect and recover from runtime degradation** |

All three use the same health check infrastructure but serve different purposes.

### What Changes

| Component | Change |
|-----------|--------|
| `orchestrator/src/health/mod.rs` | New — liveness probe loop, runs in background task |
| `orchestrator/src/health/probe.rs` | HTTP/TCP/Docker health check execution |
| `orchestrator/src/main.rs` | Start liveness probe task on startup |
| `orchestrator/src/routes/deploy.rs` | Read liveness probe config, pass to health module |
| `projects` table | Add `liveness_enabled`, `liveness_type`, `liveness_path`, `liveness_port`, `liveness_interval`, `liveness_timeout`, `liveness_failures`, `liveness_max_restarts` columns (or a separate `liveness_config` table) |
| Dashboard | Show health status badge on project cards (healthy / unhealthy / degraded) |

### Resource Impact

- One background tokio task in the orchestrator
- Health check requests are lightweight (HTTP GET or TCP connect)
- For 30 projects with 60s interval: ~30 checks/minute, negligible CPU/network
- No impact on agent — orchestrator checks via Docker API (same as existing metrics)

---

## Competitive Positioning

### Why LiteBin Over Vercel/Render

| | Vercel | Render | **LiteBin** |
|---|--------|--------|-------------|
| Cost | $20-180/mo | $7-85/mo | **Free (your server)** |
| Server | Theirs, shared | Theirs, shared | **Yours, dedicated** |
| Lock-in | High (proprietary) | Medium | **None (Docker)** |
| Deploy target | Next.js/Node primarily | Any container | **Any container + compose** |
| Multi-service | Limited | Limited | **Full compose** |
| Multi-node | No | No | **Yes (mTLS agents)** |
| Data residency | Their infra | Their infra | **Your server** |
| Sleep/wake | No | No | **Yes** |
| RAM | N/A | N/A | **~33 MB idle** |
| Backup | Their managed | Their managed | **Platform (Litestream) + Project (Rustic)** |

### Why LiteBin Over Coolify/Dokku

| | Coolify | Dokku | **LiteBin** |
|---|---------|-------|-------------|
| RAM idle | ~1.5 GB | ~50 MB | **~33 MB** |
| Builds | **On server** (Nixpacks) | **On server** (buildpacks) | **External** (CI/CLI, server just pulls) |
| Backup | Manual | Manual | **Platform (Litestream) + Project (Rustic)** |
| Migration | Not supported | Manual guide | **Planned (automated)** |
| Maintenance mode | Not supported | Not supported | **Planned** |
| Zero-downtime deploy | Partial (not compose) | Yes | **Planned** |
| Liveness probes | No | No | **Planned** |
| .env handling | Transferred over wire | N/A (git push) | **Never over network** |
| Auto-wake/sleep | No | No | **Yes** |
| Preview deploys | No | No | **Planned** |
| Multi-node | SSH | No | **Yes (mTLS agents)** |
| Dashboard | Yes | No | **Yes** |

### Implementation Order

| # | Feature | Phase | Complexity |
|---|---------|-------|------------|
| 1 | Full real-time log streaming | Quick Win | Low |
| 2 | Environment variable editor | Quick Win | Low |
| 3 | Preview environments | Phase 1 | Medium |
| 4 | App migration + duplication | Phase 2 | Medium |
| 5 | Platform backup (Litestream) | Phase 3 | Low |
| 6 | One-click apps | Phase 4 | Low |
| 7 | Deploy history + rollback | Phase 5 | Medium |
| 8 | Eject (Litebin Eject) | Phase 6 | Medium |
| 9 | Zero-downtime deploys | Phase 7 | Medium |
| 10 | Liveness probes | Phase 8 | Low |
| 11 | Master migration + promote | Phase 2 | Medium |
| 12 | GitHub App (seamless repo connect) | Future | Medium |
| 13 | Project backup (Rustic) | Planned | Medium |
| 14 | Disaster recovery | Planned | Low |
| 15 | Auto-update Docker images | Planned | Medium |
| 16 | Notifications (event outbox + router) | Planned | Low |

---

## CLI Scope (Deferred Decision)

As features grow, the CLI has 40+ planned commands across all planning docs. This is too many — most users use the dashboard, CLI is for CI/CD and occasional terminal ops. CLI-only users are a tiny minority for a self-hosted PaaS.

**Principle:** CLI covers repeatable workflows, API covers everything else.

- **CLI commands (~12-15):** `deploy`, `ship`, `login`, `logout`, `status`, `cleanup`, `config`, `preview`, `history`, `rollback`, `eject`, `backup restore`, `env set`, `logs`
- **Dashboard/API only:** registry config, notification config, auto-update config, backup config, migration, node recovery, agent reconnect, template management
- **Escape hatch:** `l8b api get/post/put/delete <endpoint>` — thin wrapper around `curl` that authenticates using stored session. Gives CLI-only users access to every API endpoint without adding subcommands for each.

To resolve: audit all planned CLI commands from planning docs, categorize as CLI / API-only, and add the `l8b api` wrapper. Not blocking any feature work.
