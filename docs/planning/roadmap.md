# Roadmap

Planned features beyond the current release. Ordered by priority and dependency.

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

```
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

Per-project GitHub config (new columns on `projects` or a separate table):

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
- **Preview list:** Expandable section under each project showing all active previews with links, status, PR number, commit SHA
- **GitHub config modal:** Enter repo URL, auto-generate webhook secret, display webhook URL to configure in GitHub
- **Preview actions:** Stop, restart, delete individual previews

### CLI Changes

```
l8b preview list <project>          -- List active previews
l8b preview delete <project> <id>   -- Delete a preview
```

### Caddy Routing

Previews use the same on-demand TLS as regular projects. No Caddy changes needed — the waker already handles catch-all subdomains. The router just needs to include preview subdomains in route resolution.

```
myapp.l8b.in           → litebin-myapp:3000         (production)
pr-42.myapp.l8b.in     → litebin-myapp-pr-42:3000   (preview)
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
| `agent/src/routes/waker.rs` | Handle preview subdomain wake (Mode B) |
| `dashboard/src/App.tsx` | Preview list in project card |
| `dashboard/src/api.ts` | Preview API calls |
| `dashboard/src/components/ProjectCard.tsx` | Preview section + GitHub config |
| `cli/src/main.rs` | Preview subcommands |
| `landing/index.html` | Add "Preview Environments" to roadmap ideas |

---

## Phase 2: App Migration

Move apps between agents with one command. Makes multi-node actually useful for rebalancing and server upgrades.

### User Experience

```bash
l8b move myapp agent-2
# → Stops on agent-1, exports volume, starts on agent-2, imports volume, updates DNS/routes
```

### Approach

1. Export volume data on source agent (existing volume export endpoint)
2. Transfer tar between agents via orchestrator (stream, not store)
3. Deploy container on target agent with imported volume
4. Update project's `node_id` in database
5. Sync Caddy routes (DNS if Cloudflare mode)
6. Clean up source container and volume

### Open Questions

- How to handle large volumes (multi-GB databases)?
- Should there be a maintenance page during migration?
- What if the target agent doesn't have enough resources?

---

## Phase 3: Backup & Restore

One-click snapshots of any app — files, database, and env vars. Download for safekeeping, upload to restore.

### User Experience

```bash
l8b backup myapp                    # Create snapshot, download tar
l8b backup myapp --upload s3:...    # Push to S3-compatible storage
l8b restore myapp backup-2024-04.tar  # Restore from file
```

### Approach

- Snapshot = tar of container filesystem + named volumes + env vars JSON
- Use Docker's built-in checkpoint or `docker export` + volume tar
- Optional push to S3/Minio for off-server storage
- Restore creates a new container from the snapshot

---

## Phase 4: Client Handover

Detach the agent and hand a running server to a client with zero LiteBin dependencies.

### User Experience

```bash
l8b detach myapp                    # Export app + agent config
# → Produces a standalone setup: Docker Compose + Caddy config
# → Client gets a running server, not a login
```

### Approach

1. Export container image + volume data
2. Generate a `docker-compose.yml` with the app's config (ports, env, volumes)
3. Generate a minimal Caddy config for the app's domain
4. Package everything into a tar that the client can `docker compose up`
5. Agent is no longer needed — pure Docker + Caddy

---

## Phase 5: App Duplication

Create isolated copies of any app with full data and config for feature branches or staging.

### User Experience

```bash
l8b clone myapp myapp-staging
# → New container, new subdomain (myapp-staging.l8b.in), copied volumes and env
```

### Approach

- Clone container from same image
- Copy named volumes
- Copy env vars (with optional overrides)
- Create new Caddy route
- Independent lifecycle from the original

---

## Quick Wins (Can Be Done Anytime)

### SEO Fix: 503 on Loading Page

The auto-wake loading page returns HTTP 200. Googlebot may index the spinner. Return `503 Service Unavailable` with `Retry-After` instead.

- `orchestrator/src/routes/waker.rs` — `loading_page_html()`
- `agent/src/routes/waker.rs` — `loading_page()`

See [todo.md](../todo.md) for details.

### Environment Variable Store

Per-project env var management via dashboard and CLI. Currently env vars are set at deploy time only.

- New `project_env_vars` table
- API endpoints: GET/PUT/DELETE per-project env vars
- Dashboard: env var editor in project settings
- CLI: `l8b env set myapp KEY=value`
- Injected at container start (no redeploy needed)
