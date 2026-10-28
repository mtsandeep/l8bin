> **Status: node-to-node migration SHIPPED (2026-10-26); duplication + export/import bundles SHIPPED (2026-10-27, shared copy engine); master migration (Phase 2/3) pending** · verified 2026-10-27

# Project Migration & Master Migration

## Overview

Two-part system:

1. **Project migration across nodes** — independent, reusable feature to move a project from one node to another
2. **Master migration flow** — migrate all projects off the master, transfer orchestrator state to an agent, promote the agent to master, then manually decommission the old master

```
Server A (master)                    Server B (agent)
┌──────────────────┐                 ┌───────────────────┐
│ orchestrator     │                 │ agent             │
│ caddy            │  ──── mTLS ───  │ agent-caddy       │
│ dashboard        │                 │ project containers│
│ (local projects) │                 │                   │
└──────────────────┘                 └───────────────────┘

Step 1: Migrate projects ──────►    (projects move A→B)
Step 2: Migrate master data ────►   (DB, settings, files)
Step 3: Promote agent ──────────►   (install.sh promote)
Step 4: Decommission A (manual)
```

---

## Phase 1: Project Migration Across Nodes (SHIPPED)

### Architecture Check

Cross-arch moves with `sha256:` (uploaded) images are rejected — redeploy from source on the target instead. Cross-arch with registry images is allowed (multi-arch images pull the right variant); unknown architecture on either side doesn't block.

### Endpoint: `POST /projects/:id/migrate`

**Request:** `{ "target_node_id": "...", "migrate_volumes": true }`

`migrate_volumes` (default true) controls whether Docker named volume contents transfer. Project files and `.env` always transfer. `sha256:` images always transfer. Registry images are never transferred — the target pulls them fresh.

**Flow:**

1. **Validate** — project exists, target online, not same node. Reject cross-arch `sha256:` moves. Return 202 immediately.
2. **Set status → `migrating`** — the migrate endpoint owns the lifecycle; reconciliation skips this status.
3. **Stop on source.**
4. **Transfer project directory** (`projects/{id}/` — compose.yaml, metadata.json, .env, bind-mount data) via `POST /volumes/export?kind=project-dir`.
5. **Transfer volumes** (if `migrate_volumes`) — each scoped volume (`litebin_{pid}_{name}`) streams as a tar.
6. **Transfer `sha256:` images** via docker save → `POST /images/load`.
7. **Clean up source** containers and networks (volumes stay — both nodes may share a Docker daemon).
8. **Switch `node_id`** in DB, clear container state, set status to `stopped`.
9. **Start on target** via the existing start path (registry pulls included).
10. **Push project-meta** to both agents, rebuild routes.

On failure before step 8: project stays intact on source (stopped), user can restart or retry migration. On failure at step 9: project is on the target in error state, user can retry start.

Source volumes are not deleted — on real servers they're harmless orphans; on shared-daemon setups deleting them would also delete the transferred copies.

### Reconciliation

`ProjectStatus::Migrating` is excluded from the reconciliation query — the migrate endpoint owns the full lifecycle.

### Volume Export/Import

Volumes transfer as single-shot tar streams over the existing mTLS channel — no chunking, no resume. Bounded by the 30-minute agent work timeout; practical ceiling is roughly 20 GB on a 100 Mbps link, more on faster links. A failed transfer restarts from zero.

**`POST /volumes/export?kind=volume&name={volume_name}`** — `agent/src/routes/volumes.rs`
- Streams a tar of the volume contents (busybox container archive API, never started).

**`POST /volumes/import?kind=volume&name={volume_name}`** — `agent/src/routes/volumes.rs`
- Body: tar stream, extracted into the volume (created if missing) via the same archive API.

Chunked, resumable transfer (100 MB chunks, resume-from-last-received) is a future enhancement for large volumes — not in Phase 1.

### Project File Transfer

The project directory (`projects/{id}/`) transfers as a single tar via `POST /volumes/export?kind=project-dir` — includes `compose.yaml`, `metadata.json`, `.env`, and any relative bind-mount data. `.env` transfers over the existing mTLS channel (same trust boundary as registry credentials).

### Image Handling

`sha256:` images (uploaded via `l8b deploy`) transfer via `docker save` → stream → `POST /images/load` on the target. Registry-referenced images are not transferred — the target pulls them fresh using the distributed registry state (credentials and mirrors included in the deploy request).

### project-meta Sync

After migration, `push_project_meta_to_agent` runs for both the source and target nodes, refreshing the metadata from the DB (the project now has the target's `node_id`, so it appears in the target's map and drops from the source's).

### DNS/Routes

- **`master_proxy` mode:** No DNS change needed. Caddy rebuilds from the DB on route sync.
- **`cloudflare_dns` mode:** Route sync updates the A record to the target node's `public_ip`.

### Network Cleanup

Source containers and networks are removed during migration (step 7 of the flow). Source volumes stay in place — on real servers they're harmless orphans; on shared-daemon setups deleting them would also delete the transferred copies.

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

| # | Task | Status |
|---|------|--------|
| 1 | Agent volume/project-dir/image export + import endpoints | ✅ Shipped |
| 2 | AgentClient streaming relay methods | ✅ Shipped |
| 3 | Transfer engine (busybox container archive API) | ✅ Shipped |
| 4 | Reconciliation skips `migrating` status | ✅ Shipped |
| 5 | `POST /projects/:id/migrate` (all directions) | ✅ Shipped |
| 6 | Dashboard migration UI (settings popover + modal) | ✅ Shipped |
| 7 | Chunked/resumable volume transfer | Future |
| 8 | Maintenance mode (503 during migration) | Future |
| 9 | Master migration (Phase 2) | Future |
| 10 | `install.sh promote` (Phase 3) | Future |
| 11 | Project duplication (clone to same/different node) | ✅ Shipped |
| 12 | Export/import bundles (cross-master move + full project backup) | ✅ Shipped |

---

## Project Duplication — SHIPPED (2026-10-27)

`POST /projects/{id}/duplicate` — implemented on the shared copy engine (`orchestrator/src/copy_engine/`) with export/import. Differences from the original sketch below: `.env` is **included by default** (toggle `include_env`) — the staging-copy use case wants a runnable copy, and the "never transfer `.env`" principle belongs to the cross-master trust boundary, where the export toggle already guards it. Same node is allowed; port-pinned (`allow_raw_ports`) / host-network copies warn instead of blocking; cross-architecture duplication is rejected for uploaded (`sha256:`) images (same rule as migration) and warned for registry refs.

The original sketch (kept for context):

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

## Export / Import Bundles — SHIPPED (2026-10-27)

Master-to-master project moves and full (cold) project backups without any
connectivity or trust between the two masters. The user carries the file.

- **Archive**: `{id}-{timestamp}.l8b.tar.gz` (gzip level 1 over a tar) =
  `manifest.json` (the durable DB shape) + `project.tar` (project dir, original
  id as top-level dir; `.env*` excluded at write time when toggled off) +
  `volumes/{scoped}.tar` + `images/{slug}.tar` (`sha256:` uploads only;
  registry refs pull fresh on import). All renaming happens at import time on
  the importing master — the archive always keeps the original id, so one
  bundle can be imported several times under different ids.
- **Endpoints**: `POST /projects/{id}/export` (job-polled via `GET
  /exports/jobs/{id}`, `exporting` project status; source stops briefly for a
  consistent snapshot and restarts), `GET/DELETE /exports/{file}` +
  `GET /exports` (folder), `POST /imports` (folder file or uploaded bundle;
  auto-suffix `-2`… on id collision; arch mismatch with embedded images
  rejected), `POST /imports/upload` (streamed raw body), `GET /imports`.
- **Folders**: `data/exports` and `data/imports` are host-bind mounted
  (`./exports`, `./imports` next to `projects/` in the compose file) so
  bundles can be scp'd in/out: `scp app.l8b.tar.gz server:~/litebin/imports/`.
  Folders are created by install/upgrade with the installing user's ownership
  (a docker-auto-created bind source would be root-owned and break scp).
- **UI**: Duplicate… / Export bundle… in project settings; Import from bundle
  page (`/manage/import-bundle`, archive icon in the home toolbar) with
  server-folder and upload tabs.
- Carried across masters: custom domain and path/alias routes (claimed on
  import unless another project already holds them — skipped with a warning).
  Not carried: deploy tokens, GitHub links (instance-level credentials). The
  rename cascade (containers,
  network, `litebin_{id}_{vol}` volumes, data dir, subdomain) is free — every
  unique name derives from the project id at run time; only the project-dir
  tar's top-level folder is re-rooted.

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
