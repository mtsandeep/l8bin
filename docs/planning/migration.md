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

### Architecture Gate

- Migration only allowed between nodes with matching `architecture` (arm64↔arm64, x86↔x86)
- Checked via `nodes.architecture` field (populated by agent health reports)

### New Orchestrator Endpoint: `POST /projects/:id/migrate`

**Request:** `{ "target_node_id": "...", "migrate_volumes": true }`

**Flow:**

1. **Validate** — project exists, target node online, same architecture, not already on target
2. **Set status → "migrating"** — with `container_id = NULL` (see reconciliation notes below)
3. **Transfer project files to target agent** — the agent reads `.env`, `metadata.json`, bind mount data, and `compose.yaml` from its own local filesystem during deploy. The orchestrator must ensure these exist on the target before deploying. This requires a new agent endpoint (see below).
4. **If `migrate_volumes: true`:** export Docker named volumes from source, import on target (see volume export/import below). Pre-create volumes on target so they're populated when containers start.
5. **If `sha256:` image:** orchestrator exports image via `docker save`, sends to agent's `POST /images/load`.
6. **Deploy on target** (without touching source) — deploy the project on the target node using existing deploy logic. The agent reads `.env` from its own filesystem, pulls images, starts containers with pre-populated volumes. The source continues running normally.
7. **Verify target** — health check: confirm all service containers are running on target, ports are mapped, public service is reachable.
8. **Update DB** — `projects.node_id` → target, `container_id`/`mapped_port` from new deploy, set `migrated = 1`, restore original status (`"running"` or `"stopped"`)
9. **Push project-meta** to target agent (add project with flags)
10. **Sync routes + DNS** — if cloudflare mode, trigger DNS sync

**Source is still running at this point.** The project is now dual-running on both source and target. The `migrated = 1` flag in the DB signals that the project has been migrated to a new node but the source has not been cleaned up yet. The source will keep running indefinitely until the user explicitly triggers cleanup.

**On migration failure (steps 3-7):** cleanup target (stop containers, remove volumes/network), restore source status, keep project on source. No data loss.

### Source Cleanup: `POST /projects/:id/migrate/cleanup`

Separate endpoint the user calls **anytime** after verifying the migration works. This is never automatic — the user decides when they're satisfied the target is working correctly.

**Only projects with `migrated = 1` can be cleaned up.** This flag is set by the migrate endpoint and cleared after successful cleanup.

**Flow:**

1. **Stop on source** — stop all service containers on the source node
2. **Push project-meta** to source agent (remove migrated project from meta)
3. **Cleanup source** — remove containers, volumes, network on source node
4. If `migrate_volumes: true` — also remove source volumes after confirming target volumes are populated
5. **Set `migrated = 0`** in DB — cleanup complete, no longer dual-running

**On cleanup failure:** source containers may be stopped but not fully removed. User can retry cleanup — the `migrated` flag remains `1` so the cleanup endpoint can be called again. The target is unaffected.

### DB Schema Addition

Add a `migrated` boolean column to the `projects` table:

```sql
ALTER TABLE projects ADD COLUMN migrated INTEGER NOT NULL DEFAULT 0;
```

- `migrated = 1` → project has been migrated to a new node, source still has running containers (dual-running state). The dashboard can show a visual indicator (e.g., "Migrated — cleanup pending").
- `migrated = 0` → normal state (default, or after cleanup completes).

### Reconciliation "migrating" Handling

**Problem:** The reconciler currently treats "migrating" identically to "deploying"/"stopping". It checks if the container is running on the assigned node, and if not, sets status to `"error"`. Since migration intentionally stops the source container before the target is running, the reconciler would incorrectly mark the project as error.

**Fix:** Update reconciliation (`orchestrator/src/nodes/reconciliation.rs`) to skip projects in "migrating" status. Migration is managed end-to-end by the migrate endpoint, not the reconciler. The migrate endpoint handles its own error recovery.

```
// reconciliation.rs — skip "migrating" projects
"SELECT * FROM projects WHERE status IN ('deploying', 'stopping') AND node_id = ?"
// Remove 'migrating' from this query — it's handled by the migrate endpoint
```

### Volume Export/Import (implement agent stubs)

**`POST /volumes/export`** — `agent/src/routes/volumes.rs`
- Request: `{ "volume_name": "litebin_mc_pgdata" }`
- Response: tar stream (`application/x-tar`)
- Implementation: create temp alpine container with volume mounted, tar contents to stdout

**`POST /volumes/import`** — `agent/src/routes/volumes.rs`
- Request: multipart with `volume_name` field + tar file body
- Response: 200 OK
- Implementation: create volume (via `docker volume create`), create temp alpine container, extract tar

### Project File Transfer

**Critical: the agent reads .env from its own filesystem, not from the deploy request.** Both `POST /containers/run` and `POST /containers/batch-run` call `read_project_env()` which reads `projects/{id}/.env` from the agent's local disk. The orchestrator never sends .env content in the deploy request. Same for `metadata.json` and bind mount data directories.

**New agent endpoint needed:** `POST /projects/:id/files`

This is a pre-deploy step — called before the actual deploy to ensure the agent has the project's files:

**Request:** multipart form with:
- `.env` file (text)
- `metadata.json` file (text, optional — for single-service)
- `compose.yaml` file (text, optional — already sent in batch-run body but good to have here for consistency)
- `bind-mount-data.tar` file (binary, optional — tar of `projects/{id}/data/` and other bind mount directories)

**Response:** 200 OK

The orchestrator reads these from its own `projects/{id}/` directory (for local projects) or from the source agent (for future agent-to-agent migration), and sends them to the target agent via mTLS before deploying.

**Local→remote (master to agent):** Orchestrator reads from its own `projects/{id}/` filesystem, sends to target agent's `POST /projects/:id/files`.

**Remote→remote (agent to agent):** Not needed for master migration. Projects already on the target agent stay there — after promotion, that agent's Docker becomes the master's Docker.

**Future (general move between two agents):** Orchestrator calls `GET /projects/:id/files/export` on source agent (returns tar of entire project directory), then sends to target agent's `POST /projects/:id/files`.

### Orchestrator Volume Helpers (for local node)

The orchestrator acts as a "local node" for volume operations when it's the source or target:

- `DockerManager::export_volume(volume_name) -> Vec<u8>` — tar the volume contents via temp container
- `DockerManager::import_volume(volume_name, tar_data)` — create/import volume from tar via temp container

### Image Handling

During the deploy-on-target step, images are pulled from the registry by default. For `sha256:` images (uploaded via `/images/load`), the agent cannot pull them — the orchestrator must export and transfer them:

- If the project's image starts with `sha256:`, the orchestrator exports it via `docker save` and sends the tar stream to the agent's `POST /images/load` before deploying.
- For registry images, let the agent pull normally (works for public and private registries the agent has access to).

For remote→remote migration (future): source agent exports via `docker save`, orchestrator proxies the stream to target agent's `/images/load`.

### project-meta Sync

After migration, the orchestrator must push updated `project-meta` to both agents:
- **Target agent:** include the migrated project with its flags (`auto_start_enabled`, `allow_raw_ports`, `allow_docker_access`)
- **Source agent:** exclude the migrated project (so source doesn't try to auto-wake it). Done after cleanup step, not during migration — source stays in meta while dual-running so it can still be stopped/restarted normally if needed.

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
3. **Export project files** — tar the orchestrator's `projects/` directory (contains compose.yaml, .env, .env.l8bin for all projects), send to agent's `POST /internal/migration/receive-files`. This is a safety net — the agent should already have these files from the project migration step, but this ensures nothing is missed.
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
| 1 | Agent volume export/import | — | Medium |
| 2 | Orchestrator volume helpers (local) | — | Low |
| 3 | Update reconciliation to skip "migrating" | — | Low |
| 4 | Agent `POST /projects/:id/files` endpoint | — | Low |
| 5 | Project migration endpoint (local→remote) | 1, 2, 3, 4 | Medium |
| 6 | Agent migration receive endpoints | — | Low |
| 7 | Master migration endpoint | 5 | Medium |
| 8 | `install.sh promote` mode | 7 | Medium |
| 9 | Dashboard UI for migration | 5 | Future |
| 10 | General agent-to-agent migration | 5 | Future |

## Verification

1. Two-node setup (same architecture): master (local) + agent
2. Deploy a compose project with volumes on local node
3. `POST /projects/:id/migrate` with `migrate_volumes: true` → verify container runs on agent, data intact, `migrated = 1` in DB, source still running
4. Verify target project works (browse to it, test functionality) — source is still running as fallback
5. `POST /projects/:id/migrate/cleanup` → verify source containers/volumes/network removed, `migrated = 0`
6. Deploy another project, migrate + cleanup it → verify both projects work on agent
7. `POST /admin/migrate-master` → verify agent receives DB, files, config
8. SSH to agent server, run `./install.sh promote` → verify orchestrator starts
9. Verify projects visible in dashboard, can start/stop/redeploy
10. Stop old master → verify everything still works
