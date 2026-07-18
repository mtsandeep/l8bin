# Project Migration & Master Migration

## Overview

Two-part system:

1. **Project migration across nodes** — independent, reusable feature to move a project from one node to another
2. **Master migration flow** — migrate all projects off the master, transfer orchestrator state to an agent, promote the agent to master, then manually decommission the old master

```
Server A (master)                    Server B (agent)
┌─────────────────┐                 ┌─────────────────┐
│ orchestrator     │                 │ agent            │
│ caddy            │    ─── mTLS ──  │ agent-caddy      │
│ dashboard        │                 │ project containers│
│ (local projects) │                 │                  │
└─────────────────┘                 └─────────────────┘

Step 1: Migrate projects ──────►    (projects move A→B)
Step 2: Migrate master data ────►   (DB, settings, files)
Step 3: Promote agent ──────────►   (install.sh promote)
Step 4: Decommission A (manual)
```

---

## Phase 1: Project Migration Across Nodes

### Architecture Check

- **Warning** (not a hard block) if source and target have different architectures (arm64↔x86)
- Checked via `nodes.architecture` field (populated by agent health reports)
- Same architecture: all migration options available (config + image + volumes)
- Different architecture: warn that volumes may contain arch-incompatible data (e.g., PostgreSQL/MySQL data directories). Recommend migrating config only and handling data transfer through the app's own export/import tools

### New Orchestrator Endpoint: `POST /projects/:id/migrate`

**Request:** `{ "target_node_id": "...", "migrate_volumes": true, "migrate_image": true, "maintenance_mode": true, "source": "live" }`

**Migration options** (user picks what to move):

| Option | Default | Description |
|--------|---------|-------------|
| Config (always) | — | compose.yaml, metadata.json, bind mount data. Required for deploy. `.env*` excluded (user places manually). |
| `migrate_image` | `false` | Transfer `sha256:` images via `docker save`/`load`. Skip if images are in a registry (target pulls them natively). |
| `migrate_volumes` | `false` | Transfer Docker named volumes. Skip for cross-arch or if user prefers app-level data migration. |
| `source` | `"live"` | `"live"` (default) — export volumes from running source agent. `"backup"` — restore volumes from Rustic backup instead (used in disaster recovery when source agent is dead). See [disaster-recovery.md](disaster-recovery.md). |

**Flow:**

1. **Validate** — project exists, target node online, not already on target. Warn if architectures differ.
2. **Set status → "migrating"** — with `container_id = NULL` (see reconciliation notes below)
3. **Transfer project config to target agent** — all `.env*` files excluded. Transfers `metadata.json`, `compose.yaml`, and bind mount data. See Project File Transfer below.
4. **Wait for user to place `.env`** — migration pauses. Dashboard shows: "Place your `.env` on the target agent at `projects/{id}/.env`, then click Continue." User handles secrets manually. `.env.l8bin` is regenerated automatically on first deploy.
5. **If `migrate_volumes: true`:** export Docker named volumes from source, import on target (see volume export/import below). Pre-create volumes on target so they're populated when containers start.
6. **If `migrate_image: true`:** orchestrator exports image via `docker save`, sends to agent's `POST /images/load`.
7. **Deploy on target** (without touching source) — deploy the project on the target node using existing deploy logic. The agent reads `.env` from its own filesystem, pulls images, starts containers with pre-populated volumes. The source continues running normally.
8. **Verify target** — health check: confirm all service containers are running on target, ports are mapped, public service is reachable.
9. **Update DB** — `projects.node_id` → target, `container_id`/`mapped_port` from new deploy, set `migrated = 1`, restore original status (`"running"` or `"stopped"`)
10. **Push project-meta** to target agent (add project with flags)
11. **Sync routes + DNS** — if cloudflare mode, trigger DNS sync
12. **If `maintenance_mode: true`:** push updated Caddy config to source agent that replaces the project's proxy routes with a `static_response` handler returning 503 + maintenance HTML page (see Maintenance Mode below). This prevents new data writes to the source, eliminating the data delta problem. The source containers stay running but are unreachable through Caddy.

**Source is still running at this point** (unless maintenance mode is enabled). The project is now dual-running on both source and target. The `migrated = 1` flag in the DB signals that the project has been migrated to a new node but the source has not been cleaned up yet. The source will keep running indefinitely until the user explicitly triggers cleanup.

**With `maintenance_mode: true`:** Source containers are running but unreachable. Any DNS-cache stragglers hitting the old server see the maintenance page. No data divergence between source and target — safe to switch DNS at any time.

**On migration failure (steps 3-7):** cleanup target (stop containers, remove volumes/network), restore source status, keep project on source. No data loss.

### Source Cleanup: `POST /projects/:id/migrate/cleanup`

Separate endpoint the user calls **anytime** after verifying the migration works. This is never automatic — the user decides when they're satisfied the target is working correctly.

**Only projects with `migrated = 1` can be cleaned up.** This flag is set by the migrate endpoint and cleared after successful cleanup.

**Flow:**

1. **Stop on source** — stop all service containers on the source node
2. **Push project-meta** to source agent (remove migrated project from meta)
3. **Cleanup source** — remove containers, volumes, network on source node
4. If `migrate_volumes: true` — also remove source volumes after confirming target volumes are populated
5. **Rebuild source Caddy config** — push updated config to source agent without the migrated project's routes (maintenance mode routes are removed along with the project)
6. **Set `migrated = 0`** in DB — cleanup complete, no longer dual-running

**On cleanup failure:** source containers may be stopped but not fully removed. User can retry cleanup — the `migrated` flag remains `1` so the cleanup endpoint can be called again. The target is unaffected.

### DB Schema Addition

Add a `migrated` boolean column to the `projects` table:

```sql
ALTER TABLE projects ADD COLUMN migrated INTEGER NOT NULL DEFAULT 0;
```

- `migrated = 1` → project has been migrated to a new node, source still has running containers (dual-running state). The dashboard can show a visual indicator (e.g., "Migrated — cleanup pending").
- `migrated = 0` → normal state (default, or after cleanup completes).

### Maintenance Mode (optional, during migration)

When `maintenance_mode: true` is passed to the migrate endpoint, the orchestrator pushes an updated Caddy config to the **source agent** after the target is verified running. This config replaces the migrated project's proxy routes with a `static_response` handler:

```json
{
    "match": [{
        "host": ["mc.example.com"],
        "not": [{ "cookie": { "l8b_maint": "<token>" } }]
    }],
    "handle": [{
        "handler": "static_response",
        "status_code": 503,
        "body": "<maintenance page HTML>"
    }]
}
```

Requests with a valid `l8b_maint` cookie bypass maintenance and reach the app normally.

**How it works:**
- The source agent's Caddy serves a 503 maintenance page for all the project's domains (subdomain, custom domain, custom routes)
- Source containers remain running but are unreachable through Caddy — no new data is written
- DNS-cache stragglers hitting the old server see the maintenance page instead of the live app
- The target server serves real traffic normally
- No data divergence between source and target — the user can switch DNS at any time

**Admin bypass (cookie-based):**
- Maintenance page HTML includes a collapsed "Admin Access" section with a token input field
- Dashboard shows the bypass token (e.g., `x7k9m2`)
- Admin enters token → JS sets `document.cookie = "l8b_maint=x7k9m2; path=/; max-age=86400"` → page reloads
- Caddy's `not` matcher sees the cookie → proxies to app normally
- Token is random, only shown in auth-protected dashboard, transmitted over HTTPS — no hashing needed

**Implementation:**
- Add `maintenance_page_html(name)` to `litebin-common/src/waker_pages.rs` — a styled 503 page ("This site is currently under maintenance.") alongside the existing loading/error/offline page templates. Includes collapsed admin bypass section with JS to set cookie and reload.
- Add `maintenance_mode` boolean column to `projects` table. When `true`, orchestrator's Caddy config builders (`routing.rs`, `cloudflare_router.rs`) generate `static_response` routes with cookie bypass instead of normal proxy routes.
- Generate a random bypass token when maintenance mode is enabled, stored in `projects` table, shown in dashboard. Token is embedded in the Caddy `not` cookie matcher and in the maintenance HTML's JS (so the admin knows what to enter — shown in dashboard, validated by Caddy via cookie match).
- For `master_proxy` mode: the master Caddy serves the 503 directly — no traffic reaches the agent
- For `cloudflare_dns` mode: the agent's Caddy directly returns 503
- Maintenance mode is cleared during the cleanup step (source Caddy config is no longer relevant after cleanup)

**Why not just stop the source containers?** Stopping containers would cause the waker to try auto-starting them (if `auto_start_enabled`). Maintenance mode at the Caddy level avoids this — containers stay running, waker is bypassed, and the response is immediate (no wake delay).

### Reconciliation "migrating" Handling

**Problem:** The reconciler currently treats "migrating" identically to "deploying"/"stopping". It checks if the container is running on the assigned node, and if not, sets status to `"error"`. Since migration intentionally stops the source container before the target is running, the reconciler would incorrectly mark the project as error.

**Fix:** Update reconciliation (`orchestrator/src/nodes/reconciliation.rs`) to skip projects in "migrating" status. Migration is managed end-to-end by the migrate endpoint, not the reconciler. The migrate endpoint handles its own error recovery.

```
// reconciliation.rs — skip "migrating" projects
"SELECT * FROM projects WHERE status IN ('deploying', 'stopping') AND node_id = ?"
// Remove 'migrating' from this query — it's handled by the migrate endpoint
```

### Volume Export/Import (chunked, resumable)

Volumes are transferred as chunked tar streams for resumability. A failed transfer restarts from the last chunk instead of re-uploading the entire volume.

**Chunk size:** 100MB default. Covers most LiteBin app volumes (typically under a few GB) with minimal chunk count while keeping progress loss acceptable on failure.

**`POST /volumes/export`** — `agent/src/routes/volumes.rs`
- Request: `{ "volume_name": "litebin_mc_pgdata" }`
- Response: tar stream (`application/x-tar`), streamed in 100MB chunks
- Implementation: create temp alpine container with volume mounted, tar contents to stdout. The tar is streamed — never fully loaded into memory. Orchestrator reads chunks and forwards to target.

**`POST /volumes/import/start`** — `agent/src/routes/volumes.rs`
- Request: `{ "volume_name": "litebin_mc_pgdata", "total_chunks": 10 }`
- Response: `{ "import_id": "abc123" }`
- Implementation: create volume (via `docker volume create`), create a temp file to accumulate chunks, return an import ID for subsequent chunk uploads

**`POST /volumes/import/chunk`** — `agent/src/routes/volumes.rs`
- Request: multipart with `import_id` field + chunk body (binary)
- Response: `{ "received": 4, "total": 10 }` (progress)
- Implementation: append chunk to temp file, track sequence number

**`POST /volumes/import/finish`** — `agent/src/routes/volumes.rs`
- Request: `{ "import_id": "abc123" }`
- Response: 200 OK
- Implementation: create temp alpine container with volume mounted, extract accumulated tar from temp file into volume, delete temp file. On failure, temp file remains for retry.

**Resume on failure:** If the orchestrator loses connection during chunk upload, it queries the import progress (`GET /volumes/import/status?import_id=abc123`) to find the last received chunk and resumes from there.

### Project File Transfer

**All `.env*` files are never transferred across the network.** This is a core LiteBin principle — `.env` files contain secrets (API keys, database passwords, tokens) and should not be sent over the wire, even over mTLS. The user is responsible for placing the `.env` file on the target agent before deployment. `.env.l8bin` (LiteBin's applied/live env) and any other `.env*` variants are also excluded — `.env.l8bin` is regenerated from `.env` on first deploy.

**Critical: the agent reads .env from its own filesystem, not from the deploy request.** Both `POST /containers/run` and `POST /containers/batch-run` call `read_project_env()` which reads `projects/{id}/.env` from the agent's local disk. The orchestrator never sends .env content in the deploy request.

**New agent endpoint needed:** `POST /projects/:id/files`

This is a pre-deploy step — called before the actual deploy to ensure the agent has the project's non-secret files:

**Request:** multipart form with:
- `metadata.json` file (text, optional — for single-service)
- `compose.yaml` file (text, optional — already sent in batch-run body but good to have here for consistency)
- `bind-mount-data.tar` file (binary, optional — tar of `projects/{id}/data/` and other bind mount directories)

**Response:** 200 OK

**.env NOT included.** The migration flow pauses after file transfer and waits for the user to confirm the `.env` is in place on the target agent's `projects/{id}/.env` path before proceeding with deployment. The dashboard can show a clear prompt: "Place your .env file on the target agent at `projects/{id}/.env`, then click Continue."

The orchestrator reads these from its own `projects/{id}/` directory (for local projects, excluding `.env*`) or from the source agent (for future agent-to-agent migration, excluding `.env*`), and sends them to the target agent via mTLS before deploying.

**Local→remote (master to agent):** Orchestrator reads from its own `projects/{id}/` filesystem (excluding `.env*`), sends to target agent's `POST /projects/:id/files`.

**Remote→remote (agent to agent):** Not needed for master migration. Projects already on the target agent stay there — after promotion, that agent's Docker becomes the master's Docker.

**Future (general move between two agents):** Orchestrator calls `GET /projects/:id/files/export` on source agent (returns tar of project directory, excluding `.env*`), then sends to target agent's `POST /projects/:id/files`.

### Orchestrator Volume Helpers (for local node)

The orchestrator acts as a "local node" for volume operations when it's the source or target:

- `DockerManager::export_volume(volume_name) -> Vec<u8>` — tar the volume contents via temp container
- `DockerManager::import_volume(volume_name, tar_data)` — create/import volume from tar via temp container

### Image Handling

When `migrate_image: true`, the orchestrator exports images via `docker save` and sends the tar stream to the agent's `POST /images/load` before deploying. This is needed for `sha256:` images (uploaded via `/images/load`) that can't be pulled from a registry.

When `migrate_image: false` (default), the target agent pulls images from the registry during deploy — works for public and private registries the agent has access to.
- For registry images, let the agent pull normally (works for public and private registries the agent has access to).

For remote→remote migration (future): source agent exports via `docker save`, orchestrator proxies the stream to target agent's `/images/load`.

### project-meta Sync

After migration, the orchestrator must push updated `project-meta` to both agents:
- **Target agent:** include the migrated project with its flags (`auto_start_enabled`, `allow_raw_ports`, `docker_observe`)
- **Source agent:** exclude the migrated project (so source doesn't try to auto-wake it). Done after cleanup step, not during migration — source stays in meta while dual-running so it can still be stopped/restarted normally if needed. If maintenance mode is enabled, the source Caddy handles the project's domains directly (503 page), so the waker/meta is less relevant but still kept for consistency.

The existing `POST /internal/project-meta` endpoint replaces the entire map, so this is safe.

### DNS/Routes

- **`master_proxy` mode:** No DNS change needed. All traffic routes through the master's Caddy which TLS-proxies to the agent. The Caddy config is rebuilt from the DB on every deploy/status change.
- **`cloudflare_dns` mode:** After updating `node_id`, trigger `sync_routes(sync_dns: true)`. This updates the A record to the target node's `public_ip`.

### Network Cleanup

Source agent's `litebin-{project_id}` Docker network is cleaned up during the **cleanup step** (not during migration). The orchestrator calls `POST {source_agent}/containers/cleanup` for the project as part of `POST /projects/:id/migrate/cleanup`. Target auto-creates the network during deploy.

---

## Phase 2: Master Data Migration

### New Orchestrator Endpoint: `POST /admin/migrate-master`

**Request:** `{ "target_node_id": "..." }`

**Preconditions:**
- No projects on local node with `node_id = 'local'` (all must be migrated and cleaned up first — `migrated` flag must be `0` or project must not exist on local)
- Target agent is online
- Same architecture

**Flow:**

1. **Validate** preconditions (reject if any local projects exist — all must be migrated and cleaned up first, no `migrated = 1` allowed)
2. **Export SQLite database** — read `data/litebin.db`, send binary to agent's `POST /internal/migration/receive-db`
3. **Export project files** — tar the orchestrator's `projects/` directory (contains compose.yaml for all projects), send to agent's `POST /internal/migration/receive-files`. `.env*` files are skipped — user must place `.env` manually on the new master after promote. This is a safety net — the agent should already have these files from the project migration step, but this ensures nothing is missed.
4. **Export config** — send orchestrator settings (DOMAIN, routing mode, public_ip, cloudflare creds, dashboard/poke subdomains) as JSON to agent's `POST /internal/migration/receive-config`
5. **Return success** with summary of what was sent

### New Agent Endpoints

**`POST /internal/migration/receive-db`**
- Receives SQLite DB file as binary body
- Stores at `data/migration/litebin.db`

**`POST /internal/migration/receive-files`**
- Receives tar of `projects/` directory
- Extracts to `data/migration/projects/`

**`POST /internal/migration/receive-config`**
- Receives JSON: `{ domain, routing_mode, public_ip, cloudflare_api_token, cloudflare_zone_id, dashboard_subdomain, poke_subdomain, ... }`
- Stores at `data/migration/config.json`

**`GET /internal/migration/status`**
- Returns what migration data has been received (db: bool, files: bool, config: bool)
- Used by promote script to verify readiness

### Key Files
- New: `orchestrator/src/routes/admin.rs` (or extend existing routes)
- Modify: `agent/src/main.rs` (register new routes)
- New: `agent/src/routes/migration.rs`

---

## Phase 3: Agent Promote to Master

### New `install.sh promote` Mode

User runs on the agent server host: `./install.sh promote`

**Flow:**

1. **Verify readiness**
   - Agent container is running
   - Migration data exists in agent's data volume (`data/migration/`)
   - All three pieces received: DB, files, config

2. **Stop agent** — `docker compose down` (agent + agent-caddy)

3. **Copy migration data** from agent volume to host
   - `data/migration/litebin.db` → `{install_dir}/orchestrator/data/litebin.db`
   - `data/migration/projects/*` → `{install_dir}/projects/`
   - Note: the agent's projects directory is already on the host at the agent's `{install_dir}/projects/` (bind-mounted into the agent container). The promote script detects this path and either uses it directly (if same install dir) or copies the migration files into the orchestrator's projects directory.

4. **Handle install directory paths** — the agent and orchestrator may be installed in different directories (e.g., `/opt/litebin/agent/` vs `/opt/litebin/`). The promote script detects the agent's install directory from its docker-compose config, and either:
   - Reuses the same directory for the orchestrator (simplest: just start the master stack there)
   - Or copies/symlinks the `projects/` directory to the orchestrator's expected path

4. **Patch the migrated database**
   ```sql
   -- All projects now run locally (the agent's Docker IS the master's Docker)
   UPDATE projects SET node_id = 'local';
   -- Remove all remote node records
   DELETE FROM nodes WHERE id != 'local';
   -- Reset local node
   UPDATE nodes SET name = 'Local', host = 'localhost', public_ip = '',
                      status = 'online', fail_count = 0 WHERE id = 'local';
   -- Clear stale container references (containers will get new IDs on first start)
   UPDATE projects SET container_id = NULL, mapped_port = NULL, status = 'stopped';
   UPDATE project_services SET container_id = NULL, mapped_port = NULL, status = 'stopped';
   -- Invalidate all sessions (different server)
   DELETE FROM tower_sessions;
   ```

5. **Generate master config**
   - Read `config.json` for DOMAIN, routing mode, etc.
   - Generate `docker-compose.yml` (master profile: orchestrator + dashboard + caddy)
   - Generate `.env` with proper config values
   - Generate `Caddyfile`

6. **Generate certificates**
   - Generate new CA + server cert (same as fresh master install)
   - Old agent certs are discarded — the new master is a fresh identity
   - If there are other agents that need to connect to the new master, their cert bundles will need to be regenerated and distributed (same as adding a new agent)

7. **Start master stack** — `docker compose --profile master up -d`

8. **Clean up agent artifacts**
   - Remove agent Docker volume (`litebin-agent-data`)
   - Remove agent config/certs from host
   - Optionally prune agent images

9. **Print next steps**
   - "Update DNS to point to this server's IP"
   - "Verify projects are accessible"
   - "You can now safely decommission the old master"

### Key Files
- Modify: `install.sh` (add `promote` mode, ~150 lines, reuses `install_master` logic)

---

## Phase 4: Decommission (Manual)

After promote succeeds, user manually:

1. Update DNS A record to point to new master's IP
2. Stop old master: `docker compose --profile master down`
3. Remove old data: `docker system prune -a`
4. Remove old install directory

---

## Implementation Order

| # | Task | Depends On | Complexity |
|---|------|-----------|------------|
| 1 | Standalone maintenance mode (DB flag, Caddy static_response, admin bypass) | — | Low |
| 2 | Agent volume export/import | — | Medium |
| 3 | Orchestrator volume helpers (local) | — | Low |
| 4 | Update reconciliation to skip "migrating" | — | Low |
| 5 | Agent `POST /projects/:id/files` endpoint | — | Low |
| 6 | Project migration endpoint (local→remote) | 1, 2, 3, 4, 5 | Medium |
| 7 | Agent migration receive endpoints | — | Low |
| 8 | Master migration endpoint | 6 | Medium |
| 9 | `install.sh promote` mode | 8 | Medium |
| 10 | Dashboard UI for migration | 6 | Future |
| 11 | General agent-to-agent migration | 6 | Future |
| 12 | Project duplication (clone to same/different node) | 6 | Future |

---

## Future: Project Duplication

Shares all the same building blocks as migration (file transfer, volume export/import, image transfer, deploy). The difference:

| | Migration | Duplication |
|---|-----------|-------------|
| Source project | Moved (node_id changes) | Unchanged (stays intact) |
| New project | N/A | Created with new name/ID |
| `migrated` flag | Set on source | N/A |
| Cleanup needed | Yes (source) | No |
| Same node allowed | No (must be different) | Yes |
| Use case | Move to new server | Staging copy, template project, multi-region |

### New Endpoint: `POST /projects/:id/duplicate`

**Request:** `{ "name": "my-app-staging", "target_node_id": "local" | "<agent_id>", "migrate_volumes": false, "migrate_image": false }`

**Flow:**

1. **Validate** — project exists, target node online, new name not taken. Warn if architectures differ.
2. **Create new project** in DB with new name, new ID, target `node_id`, status `"stopped"`
3. **Transfer project config** to target — all `.env*` files excluded (same principle as migration). Transfers `metadata.json`, `compose.yaml`, and bind mount data.
4. **Wait for user to place `.env`** on target (same as migration step 4)
5. **Export/import volumes** if `migrate_volumes: true` (same as migration step 5)
6. **Transfer image** if `migrate_image: true` (same as migration step 6)
7. **Deploy on target** — same deploy logic as migration
7. **Update DB** — set new project's `container_id`/`mapped_port`/`status` from deploy result
8. **Push project-meta** to target agent (add new project with flags)
9. **Sync routes + DNS** — if cloudflare mode, trigger DNS sync for new project

Source is never touched. No cleanup needed. The duplicated project is fully independent from the original.

### Shared Infrastructure

Both migration and duplication use the same internal helpers (extract from migration endpoint into shared functions):

- `transfer_project_files(source, target, project_id)` — file transfer
- `transfer_volumes(source, target, project_id)` — volume export/import
- `transfer_image(source, target, image_ref)` — docker save/load
- `deploy_on_target(target, project_id)` — deploy using existing logic

## Verification

1. Two-node setup (same architecture): master (local) + agent
2. Deploy a compose project with volumes on local node
3. `POST /projects/:id/migrate` with `migrate_volumes: true, migrate_image: false` → verify container runs on agent, data intact, `migrated = 1` in DB, source still running
4. Verify target project works (browse to it, test functionality) — source is still running as fallback
5. `POST /projects/:id/migrate/cleanup` → verify source containers/volumes/network removed, `migrated = 0`
6. Deploy a project with a `sha256:` image, migrate with `migrate_image: true` → verify image transferred and running on agent
7. Deploy another project, config-only migrate (`migrate_volumes: false`) → verify app starts with empty volumes, user can manually import data
8. Cross-arch test (if available): migrate with config only → verify warning shown, app deploys and runs
9. `POST /admin/migrate-master` → verify agent receives DB, files, config
10. SSH to agent server, run `./install.sh promote` → verify orchestrator starts
11. Verify projects visible in dashboard, can start/stop/redeploy
12. Stop old master → verify everything still works
