# Auto-Update Docker Images

Periodically check for new versions of `:latest` tagged images. Pull silently for stopped projects (with warm start), flag updates for running projects.

## Philosophy

LiteBin doesn't build images on the server — it pulls them. When upstream pushes a new `postgres:latest` or `traefik:latest`, the user must manually pull and redeploy. This is tedious for images that should stay current (databases, caches, reverse proxies, base images).

This feature automates the boring part (checking and pulling) while keeping the user in control (never auto-redeploy running apps).

---

## Scope

**Only `:latest` tagged images are eligible.** Everything else is pinned — never touched.

```
postgres:latest          → eligible
postgres:16              → pinned, skip
postgres:16.1            → pinned, skip
myapp:sha256:abc123      → digest, skip
ghcr.io/user/app         → no tag = :latest, eligible
ghcr.io/user/app:v2.1    → pinned, skip
```

**How to detect `:latest`:**

- Tag is literally `latest`
- No tag specified (Docker defaults to `:latest`)
- Not a digest reference (`sha256:...`)

---

## Update Modes

| Mode | When It Updates | Risk |
|---|---|---|
| `default` | Only when project is stopped (sleeping) | Zero — container isn't serving traffic |
| `force` | Even while running — stops, updates, restarts | Low — brief downtime, but waits for idle host |

**Default mode:** Projects with auto-sleep enabled naturally enter stopped state. The background task catches them during sleep windows. Projects without auto-sleep (always-on) never auto-update in default mode — user handles updates manually via dashboard.

**Force mode:** Opt-in per project. The background task stops the running container, pulls the new image, warm starts, then restarts. Still waits for idle host resources before doing anything. Brief downtime (~30-60s depending on warm start) but during low-traffic period.

**System-level rule always applies:** Updates only happen when the host has free CPU and RAM, regardless of mode.

---

## Behavior by Project State

| Project State | Check | Pull | Start | Auto-Redeploy |
|---|---|---|---|---|
| **Stopped** | Yes | Yes | Yes (warm start) | No (already stopped) |
| **Running** | Yes | No | No | No |
| **Deploying** | Skip | — | — | — |

**Running projects:** Check remote digest via registry API (lightweight, no pull). If changed, set `update_available = true` in DB and trigger a notification event ([notifications.md](notifications.md)). User decides when to redeploy.

**Stopped projects:** Pull new image, warm start (run entrypoint/init), stop. Image is ready for next wake/deploy with no initialization delay.

---

## Warm Start (Stopped Projects Only)

Some images have entrypoint scripts that run on first start (DB migrations, asset compilation, dependency install). If we just pull and leave it stopped, the user's first auto-wake pays that cost. Warm start front-loads it:

```
Pull new image
  ↓
Start container (stopped project, no traffic reaching it)
  ↓
Wait for health check (or timeout)
  ↓
Stop container
  ↓
Wait for resources to settle
  ↓
Update image reference in DB
```

**Health check during warm start:** Uses the same health check config as zero-downtime deploys (Phase 7) if configured. Falls back to "container running for N seconds" if no health check is defined.

**Warm start timeout:** 60 seconds default (configurable). If the container doesn't become healthy within the timeout, stop it anyway and mark `update_available = true` (init may have failed, user should investigate before next wake). Trigger a `warm_start_failed` notification event ([notifications.md](notifications.md)): "warm start failed for {project} after image update — check app code for compatibility."

**Database warning:** Warm starting a database image (postgres:latest, mysql:latest) runs entrypoint scripts that may perform major version migrations on data directories. This is a one-way operation. If the new major version is incompatible with the user's app, data may be corrupted. LiteBin does not block this — the user opted into `:latest` and accepts the risk. Users should pin database versions (`postgres:16`) if they want stable, predictable updates.

**What happens on next wake:** Container starts with the new image. Entrypoint has already run. No init delay — user gets instant response.

---

## Resource Management

### Sequential Updates

One project at a time. Never parallel. Wait for idle before starting the next project.

```
For each eligible project (random order):
  1. Wait for server idle (CPU < 30% and memory < 60% for 5 consecutive checks)
  2. If stopped: pull → warm start → stop
  3. If running: registry API check only
  4. Loop back to step 1 for next project
```

No fixed delay. The server decides when it's ready. A VPS with 2 idle apps finishes in minutes. A busy server with 20 apps waits for natural traffic gaps before each update.

### Load-Aware Skipping

If the server never hits the idle threshold, don't force it. Set `update_available = true` and let the user decide.

```
Before cycle:
  if CPU > 80% or memory > 85%:
    skip entire cycle, try next interval

Between projects:
  idle-wait loop (max wait: 2 hours):
    if CPU < 30% and memory < 60% for 5 consecutive checks:
      proceed with update
    if timeout reached without hitting idle:
      mark all remaining projects as update_available = true
      break, trigger notification ([notifications.md](notifications.md)): "updates pending, server too busy to auto-pull"
```

The user sees flagged projects in the dashboard and can manually pull/redeploy when convenient.

### Random Order

Projects are processed in random order each cycle. This prevents the same projects always being skipped if the cycle is interrupted by load (e.g., if the first 5 projects always run, the 6th never gets checked).

---

## Scheduling

Two-phase approach: scheduled check (cron) + background task (always running).

### Phase 1: Scheduled Check (Cron)

Runs once a day at a configured time. Uses `tokio-cron-scheduler` (same as Rustic backups). Lightweight — only registry API calls, no pulling.

```
Every day at 3 AM (configurable):
  for each eligible project (:latest tag, auto-update enabled):
    check registry manifest digest
    if digest changed from last_updated_image_ref:
      update_ready = true        -- signal the background task
      update_available = true    -- show badge in dashboard
```

**Why once a day:** Images don't update every hour. A daily check catches new versions within 24 hours — sufficient for databases, caches, and base images. The heavy work (pulling, starting) is handled by the background task.

### Phase 2: Background Update Task (Always Running)

Watches for projects marked `update_ready`. Waits for the right conditions (stopped + idle) before doing the actual work. This runs as a persistent tokio task in the orchestrator.

```
Loop every 1 minute:
  for each project where update_ready = true:

    skip if status = "running" AND auto_update_force = 0

    wait for idle (CPU < 30% and memory < 60% for 5 consecutive checks)
    if idle timeout reached (2 hours):
      break, leave update_ready = true, try again next loop

    if status == "running":
      stop container

    pull image
    warm start (start → health check → stop)

    if auto_update_force = 1:
      start container (back to running)

    update_ready = false
    last_updated_image_ref = new digest
```

**Why a background task instead of cron-only:** Apps may be stopped and sleeping for brief, unpredictable windows during the day (auto-sleep after 30 min of inactivity). A cron job at 3 AM might miss that window if the app is running at 3 AM but sleeps at 2 PM. The background task catches the window whenever it happens.

**For running projects:** The cron check flags `update_available = true` in the dashboard. The background task never touches running projects. User decides when to redeploy.

**Projects that never hit idle:** Stay flagged as `update_ready`. Next day's cron re-checks and refreshes the flag. The user can also manually pull from the dashboard.

---

## Configuration

### Global Settings

```sql
CREATE TABLE auto_update_config (
  id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0,
  check_cron TEXT NOT NULL DEFAULT '0 3 * * *',      -- daily registry check at 3 AM
  idle_cpu_percent INTEGER NOT NULL DEFAULT 30,      -- CPU below this = idle
  idle_memory_percent INTEGER NOT NULL DEFAULT 60,   -- memory below this = idle
  idle_consecutive_checks INTEGER NOT NULL DEFAULT 5, -- must be idle for N checks (1 min each)
  idle_wait_minutes INTEGER NOT NULL DEFAULT 120,    -- max wait for idle before giving up per project
  warm_start_timeout_seconds INTEGER NOT NULL DEFAULT 60,
  max_cpu_percent INTEGER NOT NULL DEFAULT 80,       -- skip cycle if CPU above this
  max_memory_percent INTEGER NOT NULL DEFAULT 85,    -- skip cycle if memory above this
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Per-Project Override

```sql
ALTER TABLE projects ADD COLUMN auto_update_enabled INTEGER;
ALTER TABLE projects ADD COLUMN auto_update_force INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN update_available INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN last_update_check_at TEXT;
ALTER TABLE projects ADD COLUMN last_updated_image_ref TEXT;
```

- `auto_update_enabled`: NULL = inherit global, 0 = disabled, 1 = enabled
- `auto_update_force`: 0 = default (update only when stopped), 1 = force (update even while running)
- `update_available`: set to 1 when a new image is detected
- `last_update_check_at`: last time this project was checked
- `last_updated_image_ref`: the image digest/reference after last successful update

---

## Registry API Check

For running projects, only the remote manifest is fetched — no image pull. This is lightweight (a few KB API call).

```
GET /v2/{name}/manifests/{reference}
→ returns digest (Docker-Content-Digest header)

GET /v2/{name}/tags/list
→ returns all available tags (for version catalog)
```

Compare with `last_updated_image_ref` in DB. If different → `update_available = true`.

**Rate limiting:** Some registries rate-limit anonymous API calls. Docker Hub allows 200 pulls/6h for anonymous, unlimited for authenticated. A daily check per project stays well within limits.

---

## Private Registry Authentication

LiteBin's registry API calls are direct HTTP from the orchestrator/agent to the registry — they don't go through Docker. Public images work without auth. Private registries need credentials configured in LiteBin.

### How It Works

Users add registry credentials in the dashboard (or CLI). LiteBin stores them and matches them against image URLs from the project DB.

```
Image from project DB: ghcr.io/myorg/myapp:latest
                         ↓
Match against registry configs by URL prefix
                         ↓
Found match: ghcr.io → use stored credentials
                         ↓
API call: GET https://ghcr.io/v2/myorg/myapp/manifests/latest
         Authorization: Bearer <token>
```

### Registry Config

Global registry credentials stored in the orchestrator DB. Any number of registries can be added.

```sql
CREATE TABLE registry_auth (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,                    -- display name (e.g., "My GHCR")
  registry_url TEXT NOT NULL UNIQUE,     -- e.g., "ghcr.io", "123456789.dkr.ecr.us-east-1.amazonaws.com"
  auth_type TEXT NOT NULL,               -- "bearer", "basic", "ecr"
  username TEXT,                          -- for basic auth
  password TEXT,                          -- for basic auth
  token TEXT,                             -- for bearer auth (GHCR, GitLab, Gitea)
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Matching logic:** When checking an image, extract the registry URL from the image reference and match against `registry_auth.registry_url` by prefix:

```
Image: ghcr.io/myorg/myapp:latest
  → registry host: ghcr.io
  → match: ghcr.io in registry_auth → use those credentials

Image: 123456789.dkr.ecr.us-east-1.amazonaws.com/myapp:latest
  → registry host: 123456789.dkr.ecr.us-east-1.amazonaws.com
  → match: 123456789.dkr.ecr.us-east-1.amazonaws.com in registry_auth
```

**No match:** Treat as public registry, call API without auth. If it returns 401, flag as auth error in the version catalog (don't silently fail).

### Auth Types

| Auth Type | Use Case | Config |
|-----------|----------|--------|
| `bearer` | GHCR, GitLab, Gitea, Harbor, Quay | Token only |
| `basic` | Self-hosted `registry:2`, Harbor (some setups) | Username + password |
| `ecr` | AWS ECR | AWS access key + secret key (LiteBin calls ECR GetAuthorizationToken API to get temporary bearer token) |

### Common Registry Setup

| Registry | Auth Type | How to Get Token |
|----------|-----------|-----------------|
| Docker Hub (private) | `bearer` | Docker Hub access token |
| GHCR (private) | `bearer` | GitHub Personal Access Token (package:read scope) |
| GitLab Container Registry | `bearer` | GitLab Personal Access Token (read_api or read_registry) |
| Gitea / Forgejo | `bearer` | Gitea personal access token |
| Harbor | `bearer` or `basic` | Robot account token |
| AWS ECR | `ecr` | IAM access key with ecr:GetAuthorizationToken permission |
| Self-hosted `registry:2` | `basic` | htpasswd username/password |
| GCR / GAR | `bearer` | Google service account JSON key (parsed to extract token) |

### Security

- Passwords/tokens are stored in the SQLite DB. The DB itself is protected by Litestream backup (encrypted at rest via S3 SSE) and server-level security (LUKS, SSH hygiene).
- Credentials are only used for registry API calls (manifest check, tag list). They are never logged, never included in responses, never sent to agents (registry API calls happen from the orchestrator, not agents — orchestrator passes the results to agents that need to pull).
- Dashboard shows registry name and URL but masks the token/password.
- ECR tokens are temporary — LiteBin calls `GetAuthorizationToken` before each check cycle, gets a 12-hour bearer token, uses it for API calls, discards it.

### Dashboard

Settings page shows configured registries:

```
Registry Credentials
─────────────────────────────────────────────────────
Registry              URL                                              Auth    Status
─────────────────────────────────────────────────────
My GHCR               ghcr.io                                          Bearer  ✓ (connected)
AWS ECR               123456789.dkr.ecr.us-east-1.amazonaws.com        ECR     ✓ (connected)
My Harbor             harbor.mycompany.com                             Basic   ✓ (connected)

[+ Add Registry]
```

Clicking a registry shows: edit credentials, test connection, delete.

### CLI

```bash
# Add a registry
l8b registry add ghcr.io --token ghp_xxxxx --name "My GHCR"

# Add AWS ECR
l8b registry add 123456789.dkr.ecr.us-east-1.amazonaws.com --ecr --access-key AKIA... --secret-key xxxxx

# Add self-hosted registry with basic auth
l8b registry add harbor.mycompany.com --username robot --password xxxxx

# List configured registries
l8b registry list

# Test connection
l8b registry test ghcr.io

# Remove
l8b registry remove ghcr.io
```

### API Endpoints

```
GET    /admin/registries              — List configured registries (credentials masked)
POST   /admin/registries              — Add registry
PUT    /admin/registries/:id          — Update registry credentials
DELETE /admin/registries/:id          — Remove registry
POST   /admin/registries/:id/test     — Test registry connection
```

---

## Dashboard

### Running Project with Update Available

Project card shows a badge:

```
┌─────────────────────┐
│ myapp          🔄 v2 │  ← "Update available"
│ Running · 256 MB     │
└─────────────────────┘
```

Clicking the badge shows: current image ref, new image ref, last checked, and a "Pull & Redeploy" button.

### Update Log

Settings page shows recent update activity:

```
Auto-Update Log
─────────────────────────────────────────────
postgres   latest → digest:abc123  2h ago  ✓ (warm started)
redis      no update available     2h ago  —
myapp      update available        2h ago  ⚠ (running, not pulled)
traefik    latest → digest:def456  3h ago  ✓ (warm started)
```

---

## CLI

```bash
# Check status
l8b auto-update status

# Check a specific project
l8b auto-update check myapp

# Trigger manual update (pull + warm start for stopped, or pull for running)
l8b auto-update run myapp

# Enable/disable globally
l8b auto-update enable
l8b auto-update disable

# Configure check time
l8b auto-update config --time "03:00"
```

---

## API Endpoints

```
GET  /admin/auto-update/config         — Get global config
POST /admin/auto-update/config         — Update global config
GET  /admin/auto-update/log            — Recent update activity
POST /admin/auto-update/run            — Trigger manual update cycle
GET  /projects/:id/update-status       — Check update status for a project
POST /projects/:id/update              — Pull & optionally redeploy
```

---

## What This Doesn't Do

| Doesn't | Why |
|---------|-----|
| Auto-redeploy running projects (default mode) | User decides when to redeploy. Never surprise-break production. Force mode opt-in overrides this. |
| Update pinned tags (`:16`, `:v2.1`) | Pinned for a reason. User controls pinning. |
| Update `sha256:` digest refs | These are intentionally pinned to a specific build. |
| Build images | LiteBin doesn't build on server. This is pull-only. |
| Update during auto-wake | User is waiting. No extra latency on user-facing requests. |
| Update during deploy | Deploy has its own flow. Don't interfere. |

---

## Implementation Order

| # | Task | Complexity |
|---|---|---|
| 1 | DB migration: `auto_update_config`, project columns | Low |
| 2 | Image tag detection (latest vs pinned vs digest) | Low |
| 3 | Registry API manifest check (lightweight, no pull) | Medium |
| 4 | Background task: sequential update loop with load checks | Medium |
| 5 | Warm start flow (pull → start → health check → stop) | Medium |
| 6 | Dashboard: update badge, update log | Low |
| 7 | CLI commands | Low |
| 8 | API endpoints | Low |

Items 1-3 are the core. Items 4-5 add the warm start and resource management. Items 6-8 are UI/UX.

---

## Resource Impact

| Component | Impact |
|-----------|--------|
| Background task (always running) | ~2 MB (tokio task, polls every 1 min) |
| Cron task (during check) | ~0 MB (reuses scheduler, registry API calls only) |
| Registry API check | ~1 KB per check, ~20 checks/cycle = ~20 KB |
| Image pull (stopped project) | Disk I/O + network, one at a time |
| Warm start | Container RAM during start (~100-500 MB briefly), one at a time |
