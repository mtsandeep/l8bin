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
| Future: external builders | Dedicated build service (separate from VPS) | Not planned — adds complexity |

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

## Phase 3: Backup

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

## Phase 4: One-Click Apps

Template catalog of popular apps deployable with a single click. Since LiteBin supports docker-compose, one-click apps are custom compose files (official or user-contributed).

### User Experience

```
1. Dashboard → "New Project" → "From Template"
2. Browse catalog (WordPress, Next.js, n8n, Mastodon, etc.)
3. Click "Deploy" → configure domain + env vars
4. Running
```

### Approach

- Templates are docker-compose files stored in a registry (GitHub repo or in-app)
- Official templates maintained by LiteBin
- Users can submit custom templates
- Template may include default env vars, volume mounts, health checks
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

Deploy success/fail notifications via Discord/Slack/webhook.

- Per-project notification config (provider, webhook URL)
- Events: deploy start, deploy success, deploy failure, auto-stop, auto-wake
- Dashboard: notification settings in project config

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
| Backup | Their managed | Their managed | **Litestream (your storage)** |

### Why LiteBin Over Coolify/Dokku

| | Coolify | Dokku | **LiteBin** |
|---|---------|-------|-------------|
| RAM idle | ~1.5 GB | ~50 MB | **~33 MB** |
| Builds | **On server** (Nixpacks) | **On server** (buildpacks) | **External** (CI/CLI, server just pulls) |
| Backup | Manual | Manual | **Litestream (real-time)** |
| Migration | Not supported | Manual guide | **Planned (automated)** |
| Maintenance mode | Not supported | Not supported | **Planned** |
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
| 5 | Backup (Litestream integration) | Phase 3 | Low |
| 6 | One-click apps | Phase 4 | Low |
| 7 | Deploy history + rollback | Phase 5 | Medium |
| 8 | Eject (Litebin Eject) | Phase 6 | Medium |
| 9 | Notifications | Quick Win | Low |
| 10 | Master migration + promote | Phase 2 | Medium |
| 11 | GitHub App (seamless repo connect) | Future | Medium |
