# Project Backup (Rustic)

Incremental backup of project data — Docker volumes, compose files, and user-uploaded images — using Rustic.

## Context

LiteBin currently backs up only the SQLite database via Litestream ([backup.md](backup.md)). Project data — Docker volumes, compose files, and user-uploaded images — has no backup story. This plan adds incremental project backups with configurable retention policies, local + external storage (S3/SFTP), and a separate image vault for user-uploaded `sha256:` images.

## Tool: Rustic

[Rustic](https://github.com/rustic-rs/rustic) is a Rust-native reimplementation of Restic with the same repo format. Shell out via `std::process::Command` from Rust, parse `--json` output with `serde_json`.

**Why Rustic over Restic:**

| | Rustic | Restic |
|--|--------|--------|
| Language | Rust (no GC, zero runtime overhead) | Go (garbage-collected) |
| Backup RAM (~10 GB repo) | **~15–30 MB** | 54–111 MB |
| Prune RAM | **~15–30 MB** | 100–200 MB (can spike with GC) |
| Locking | **Lock-free** (all ops concurrent) | Exclusive lock on prune/check |
| Index memory | **1x theoretical size** | 3–4x theoretical (Go GC bloat) |
| Rust library | **Yes (`rustic_core`)** | No |
| Repo format | **Restic-compatible** | Original |
| SFTP | Built-in via opendal | External `ssh` binary |
| Extra backends | Dropbox, FTP, GDrive, OneDrive, WebDAV | — |
| Retention options | hourly/daily/weekly/monthly/quarterly/half-yearly/minutely | hourly/daily/weekly/monthly |
| Config profiles | In-repo config (set once) | Pass flags every time |
| Hooks | Pre/post backup commands | No |
| Stars | 3k+ | 28k+ |
| Maturity | Beta (v0.10) | Stable (v0.18) |

**Why the beta status is not a concern for LiteBin:**
- We shell out to CLI (not using as a library) — pin the binary version, no code coupling
- Repos are restic-compatible — if rustic has a critical bug, swap to restic CLI with zero data migration
- LiteBin's ~10 GB scale won't hit edge cases that affect multi-TB repos
- Core commands (backup, restore, forget, snapshots, init) are well-tested even in beta
- Actively maintained with regular releases

**Rustic binary:** Included in both orchestrator and agent Docker images. No separate container needed.

---

## Resource Usage & Performance

### RAM Usage (LiteBin Scale: ~10 GB Total Data)

Rustic memory scales with the **number of blobs in the index**, not with total data size. Rustic's index uses exactly the theoretical size (no GC overhead), and loads only the index type needed per operation (e.g., backup only needs blob IDs, not positions).

| Operation | Rustic RAM | Restic RAM |
|-----------|-----------|------------|
| Backup | **~15–30 MB** | 54–111 MB |
| Prune | **~15–30 MB** | 100–200 MB (GC spikes) |
| Restore | **~15–30 MB** | ~80–154 MB |
| Check | **~15–30 MB** | 100–200 MB |
| Idle | **0 MB** | 0 MB |

**Impact on LiteBin total:**

| Component | RAM |
|-----------|-----|
| Orchestrator | ~8.5 MB |
| Dashboard | ~4.6 MB |
| Caddy | ~20 MB idle |
| Litestream | ~5–10 MB |
| **LiteBin total (idle)** | **~38–43 MB** |
| Rustic (during backup) | **~15–30 MB** |
| **During backup** | **~55–75 MB** |

Rustic runs as a short-lived CLI process — spawned, runs the backup, exits. Zero RAM between backups.

### CPU & Disk I/O

- Backup is I/O-bound, not CPU-bound
- Content-defined chunking (Rabin fingerprints) — minimal CPU overhead
- No Go garbage collector pauses
- Deduplication means incremental backups only read/write changed data

### Locking & Concurrency

**Rustic is lock-free** — all operations (backup, prune, check, restore) can run concurrently. No stale locks, no "failed to refresh lock in time" errors, no need to schedule prune separately.

### Restore Speed

- Local disk: ~50–100 MB/s
- Cloud backend (S3/SFTP): ~15–50 MB/s (network-bound)
- Resumable restores supported

---

## Large File Handling

### How Rustic Handles Large Files

Rustic **does not load entire files into memory**. Streaming approach:

1. **Content-defined chunking (Rabin fingerprints):**
   - Average chunk size: **1 MiB** (default, configurable in rustic via `rustic config --set-chunk-size`)
   - Minimum: 512 KiB, Maximum: 8 MiB
   - Memory per chunk tracked: ~56 bytes
   - **Why 1 MiB is right for LiteBin:** A 1 KB change in a 2 GB volume only re-uploads ~1 MiB. Larger chunks (4–16 MiB) would re-upload 4–16x more data per change. At 10 GB total, the index overhead is ~2 MB — negligible. This is the right tradeoff for small VPS with limited bandwidth.

2. **Streaming pipeline:** File → chunker → compressor → encryptor → backend. Each chunk processed and flushed independently. Entire file never in memory.

3. **Incremental efficiency:** Only changed chunks are stored. A 5 GB volume where 50 MB changed = only ~50 new chunks.

### Docker Volume Export Strategy

Avoid intermediate tar — backup the directory tree directly:

```
docker run --rm -v litebin_abc123_pgdata:/data alpine
→ docker cp contents to /tmp/backup-{id}/volumes/pgdata/
→ rustic backup /tmp/backup-{id}/ --tag project:abc123
```

Each file chunked independently = better dedup than a monolithic tar. Single file change in a 5 GB volume only re-uploads that file's chunks.

### Image Vault Tars

Image tars from `docker save` (500 MB–2 GB) handled the same way:
- Saved to disk first (`data/image-vault/...`), backed up by rustic on next scheduled backup
- Chunked at 1 MiB boundaries — shared image layers deduplicated across tars
- No memory concern — streamed through chunking pipeline

### Database Volume Consistency

For volumes with live databases (PostgreSQL, MySQL), raw file backup can produce inconsistent snapshots. Two approaches:

1. **Recommended:** User runs `pg_dump`/`mysqldump` via rustic's pre-backup hooks to a known path before backup runs
2. **Simple (current scope):** Back up raw volume data. Works for stopped/read-only databases. For running databases, user handles dumps manually — documented in dashboard

---

## Architecture: Hybrid

```
┌─────────────────────────────────────────────────────┐
│                  Orchestrator (Master)               │
│                                                      │
│  ┌──────────────┐  ┌────────────┐  ┌─────────────┐ │
│  │ Backup        │  │ Scheduler  │  │ Rustic CLI  │ │
│  │ Manager       │  │ (tokio-    │  │ (local      │ │
│  │               │  │  cron)     │  │  projects)  │ │
│  └──────┬───────┘  └─────┬──────┘  └──────┬──────┘ │
│         │                │                 │        │
│         │    HTTP (mTLS) │                 │        │
└─────────┼────────────────┼─────────────────┼────────┘
          │                │                 │
          ▼                ▼                 ▼
┌─────────────────┐  ┌─────────────────┐
│   Agent A       │  │   Agent B       │
│ ┌─────────────┐ │  │ ┌─────────────┐ │
│ │ Rustic CLI  │ │  │ │ Rustic CLI  │ │
│ │ + Backup    │ │  │ │ + Backup    │ │
│ │   Endpoints │ │  │ │   Endpoints │ │
│ └─────────────┘ │  │ └─────────────┘ │
│ Rustic repo:    │  │ Rustic repo:    │
│ local + S3      │  │ local + SFTP    │
└─────────────────┘  └─────────────────┘
```

- **Local projects:** Orchestrator shells out to rustic directly (has DockerManager access)
- **Remote projects:** Orchestrator calls agent's `POST /internal/backup/project/{id}` via mTLS; agent runs rustic locally
- **Each node has its own rustic repo** — local path + optional S3/SFTP
- **Orchestrator manages scheduling and policy**, agents execute

---

## Image Vault (Separate from Volume Backup)

**Scope:** Only `sha256:` (user-uploaded) images. Registry images are never backed up — they can be re-pulled.

**How it works:**
1. User uploads image via `POST /images/upload` (existing endpoint)
2. **Before loading into Docker**, save the tar to the image vault: `data/image-vault/{project_id}/{service_name}/{timestamp}_{short_digest}.tar`
3. Then load into Docker as normal (existing flow)
4. Each unique uploaded image gets one saved tar — no duplicates
5. Vault files are included in the rustic backup (tagged separately)

**Restore (independent of volume restore):**
- Dashboard shows image history per project/service
- User picks a previous image → orchestrator loads the vault tar into Docker via agent's `POST /images/load`
- Updates `project_services.image` to the restored image ref
- User can then redeploy with the restored image

**This is not "backup on schedule"** — images are saved on every upload. The vault is a version history, not a scheduled backup.

### Image Vault vs Private Registry

Users who want proper image management can already use a private registry (e.g., GitHub Container Registry, Docker Hub, Harbor) from their CI/CD pipeline — GitHub Actions builds and pushes to GHCR, LiteBin pulls the image by tag. This is the **recommended approach for registry images**. The image vault only covers `sha256:` images that are manually uploaded through the dashboard (no registry involved).

| | Image Vault | Private Registry (GHCR, Harbor, etc.) |
|--|-------------|---------------------------------------|
| **Covers** | Only `sha256:` (manually uploaded) images | Registry images (built via CI/CD, pushed to registry) |
| **How it works** | `docker save` → tar to disk → rustic backup | `docker push` to registry → `docker pull` to deploy |
| **Layer dedup** | Partial (rustic chunk-level) | **Full** (same base layer stored once) |
| **Pull from any agent** | No — orchestrator sends tar | **Yes** — `docker pull` from anywhere |
| **Multi-project sharing** | Each project stores its own tar | **One copy** shared across projects |
| **Image metadata** | Just filename + DB record | **Full** Docker manifest, tags, labels |
| **Rollback** | Pick from list in dashboard | `docker pull app:v1.2` — standard workflow |
| **Extra service needed** | **None** | Registry (but user manages it, not LiteBin) |
| **Backup** | **Automatic** (included in rustic backup) | User's responsibility (registry storage) |
| **Disk usage** | Higher (full tars, rustic dedup helps) | **Lower** (layer sharing) |
| **Who manages it** | LiteBin (built-in) | **User** (external service) |

**Why vault is the right fit for sha256: uploads:** These are images with no registry — the user built them locally or got them as tar files. There's nowhere to `docker pull` from. The vault is the only way to preserve them. Registry images don't need the vault because they already have a home — the registry itself is the backup.

---

## DB Schema

### `backup_config` (global, singleton)

```sql
CREATE TABLE backup_config (
  id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0,
  schedule_cron TEXT NOT NULL DEFAULT '0 2 * * *',
  retention_hourly INTEGER NOT NULL DEFAULT 0,
  retention_daily INTEGER NOT NULL DEFAULT 7,
  retention_weekly INTEGER NOT NULL DEFAULT 4,
  retention_monthly INTEGER NOT NULL DEFAULT 0,
  storage_type TEXT NOT NULL DEFAULT 'local',
  local_path TEXT NOT NULL DEFAULT '/app/data/backups/rustic',
  s3_endpoint TEXT,
  s3_bucket TEXT,
  s3_prefix TEXT NOT NULL DEFAULT 'litebin/',
  s3_access_key TEXT,
  s3_secret_key TEXT,
  sftp_host TEXT,
  sftp_port INTEGER NOT NULL DEFAULT 22,
  sftp_user TEXT,
  sftp_path TEXT NOT NULL DEFAULT '/backups/litebin/',
  sftp_password TEXT,
  sftp_key TEXT,
  rustic_password TEXT NOT NULL,
  include_image_vault INTEGER NOT NULL DEFAULT 1,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### `project_backup_overrides` (per-project)

```sql
CREATE TABLE project_backup_overrides (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  enabled INTEGER,
  schedule_cron TEXT,
  retention_hourly INTEGER,
  retention_daily INTEGER,
  retention_weekly INTEGER,
  retention_monthly INTEGER,
  include_image_vault INTEGER,
  last_backup_at TEXT,
  last_snapshot_id TEXT,
  last_backup_status TEXT,
  last_backup_error TEXT,
  backup_size_bytes INTEGER
);
```

### `image_vault`

```sql
CREATE TABLE image_vault (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  service_name TEXT NOT NULL,
  image_ref TEXT NOT NULL,
  tar_filename TEXT NOT NULL,
  file_size_bytes INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

---

## Orchestrator Endpoints (new)

```
# Global project backup config
GET  /admin/backup/project/config            -- Get config (sensitive fields masked)
POST /admin/backup/project/config            -- Update config
POST /admin/backup/project/init              -- Initialize rustic repo (first-time setup)
GET  /admin/backup/project/status            -- Overall status (repo stats, last backup times)
POST /admin/backup/project/test-connection   -- Test storage backend connectivity

# Per-project backup
GET  /projects/{id}/backup                   -- Project backup status + snapshot list
POST /projects/{id}/backup                   -- Trigger immediate backup
PATCH /projects/{id}/backup/override         -- Set per-project override
DELETE /projects/{id}/backup/override        -- Remove override (inherit global)
POST /projects/{id}/backup/restore           -- Restore from snapshot (body: { snapshot_id })

# Image vault
GET  /projects/{id}/images/vault             -- List vault entries for project
DELETE /projects/{id}/images/vault/{vault_id} -- Delete vault entry + tar file
POST /projects/{id}/images/vault/{vault_id}/restore -- Restore image from vault
```

## Agent Endpoints (new)

All under mTLS-protected internal routes.

```
POST /internal/backup/init                   -- Initialize local rustic repo
POST /internal/backup/configure              -- Receive backup config from orchestrator
POST /internal/backup/project/{id}           -- Trigger backup for a project
GET  /internal/backup/project/{id}/snapshots -- List snapshots for a project
POST /internal/backup/project/{id}/restore   -- Restore project from snapshot
POST /internal/backup/forget                 -- Run retention policy (prune old snapshots)
GET  /internal/backup/status                 -- Repo status (size, snapshot count)
GET  /internal/backup/stats                  -- Detailed repo statistics
```

---

## Backup Flow

### Local Project (orchestrator runs directly)

1. Orchestrator resolves effective backup config (global + per-project override)
2. Create temp directory: `/tmp/backup-{project_id}/`
3. Copy project files to temp dir:
   - `compose.yaml` (from `projects/{id}/compose.yaml`)
   - `metadata.json` (from `projects/{id}/metadata.json`, if single-service)
   - **Exclude all `.env*` files**
4. For each project volume (from `project_volumes` table):
   - Use DockerManager to create a temp alpine container with the volume mounted
   - `docker cp` volume contents to temp dir at `volumes/{volume_name}/`
   - Remove temp container
5. If `include_image_vault` enabled: copy vault tars from `data/image-vault/{project_id}/`
6. Shell out to rustic:
   ```bash
   rustic -r {repo} backup /tmp/backup-{id} \
     --tag project:{id} \
     --tag type:project \
     --json
   ```
7. Parse JSON output (snapshot ID, files added, size, duration)
8. Run retention policy:
   ```bash
   rustic -r {repo} forget \
     --keep-hourly {N} --keep-daily {N} --keep-weekly {N} --keep-monthly {N} \
     --group-by tag --tag project:{id} \
     --prune
   ```
9. Clean up temp directory
10. Update `project_backup_overrides` with result

### Remote Project (orchestrator → agent)

Same logic, but orchestrator sends `POST /internal/backup/project/{id}` to the agent. The agent:
- Reads project files from its own `projects/{id}/` directory
- Exports volumes using its local Docker access
- Runs rustic locally
- Returns result to orchestrator

Agent receives config from orchestrator on startup or when config changes:
- `POST /internal/backup/configure` sends storage type, paths, credentials, rustic password
- Agent stores config in memory (not persisted — it's stateless, receives from orchestrator each time)

### What Gets Backed Up

| Data | Included | Notes |
|------|----------|-------|
| Docker named volumes | Yes | Exported via temp container + docker cp |
| Bind mount data | Yes | Copied from `projects/{id}/data/` and other bind dirs |
| compose.yaml | Yes | From project directory |
| metadata.json | Yes | For single-service projects |
| Image vault tars | Yes (if enabled) | sha256: image tars from vault directory |
| `.env*` files | **No** | LiteBin principle — never transferred/stored |
| Docker images | **No** | Handled separately by image vault (sha256: only) |
| Container state | **No** | Containers are recreated on restore, not restored |

---

## Restore Flow

### Volume + Config Restore

1. User selects snapshot from dashboard: `POST /projects/{id}/backup/restore { snapshot_id }`
2. Orchestrator stops project containers (existing `POST /projects/{id}/stop`)
3. **For local:** shell out to `rustic restore {snapshot_id} --target /tmp/restore-{id}`
   **For remote:** call `POST /internal/backup/project/{id}/restore` on agent
4. Agent/orchestrator:
   a. Restore compose.yaml + metadata.json to `projects/{id}/`
   b. For each volume directory in restore:
      - Create temp alpine container with volume mounted
      - `docker cp` contents from restore dir into the volume
      - Remove temp container
5. Redeploy project (existing deploy logic)
6. Update backup status in DB

### Image Vault Restore

1. User views image history: `GET /projects/{id}/images/vault`
2. User clicks "Restore" on a vault entry: `POST /projects/{id}/images/vault/{vault_id}/restore`
3. Orchestrator reads tar file from vault directory
4. For local: `docker load < {tar_path}`
   For remote: sends tar to agent's `POST /images/load`
5. Updates `project_services.image` to the vault entry's `image_ref`
6. User can then redeploy with the restored image

---

## Scheduling

Use `tokio-cron-scheduler` crate in the orchestrator.

**Sequential execution:** Projects are backed up one at a time, not in parallel. Reasons:
- Each rustic process loads the index (~15–30 MB) — parallel = RAM multiplied
- Disk I/O contention — reading multiple volumes simultaneously kills performance on small VPS
- S3 backend has connection limits
- LiteBin servers typically have 10–30 projects — sequential completes in minutes

On startup:
1. Load global `backup_config`
2. Load all `project_backup_overrides`
3. Register a single cron job per unique schedule (most projects share the global schedule)
4. On each cron tick, collect all due projects, sort by last backup time (oldest first), execute sequentially
5. Each project backup: resolve effective config (override > global) → run backup → run retention policy (`rustic forget --prune`) → update status
6. Since rustic is **lock-free**, prune runs alongside any other operation — no scheduling concerns

On config change (via dashboard or API):
1. Reload all schedules
2. Remove old jobs, add new ones with updated config

Cron expression format: standard 5-field (`minute hour day month weekday`).
Defaults: `0 2 * * *` (daily at 2 AM). Staggered from platform backup (3 AM) and auto-update check (3 AM) to avoid simultaneous resource contention.

**Backup queue state:** If a scheduled backup is already running when the next cron tick fires, the tick is skipped (no queue buildup). A `backup_running` flag prevents overlapping runs.

---

## Storage Backends

### Local
- Default. Rustic repo at `data/backups/rustic/`
- No external dependencies
- Suitable for single-server setups

### S3 (S3-compatible: AWS, Cloudflare R2, MinIO, Wasabi)
- Configured via dashboard: endpoint, bucket, prefix, access key, secret key
- Rustic repo stored at `s3:{endpoint}/{bucket}/{prefix}`
- Same dedup/encryption as local
- Can coexist with local (rustic supports multiple repos)

### SFTP
- Configured via dashboard: host, port, user, path, password or SSH key
- Rustic repo stored at `sftp:{user}@{host}:{path}` (native, no external ssh binary)
- Suitable for backing up to another VPS

### Multi-backend (future)
- Not in initial scope, but rustic supports `rustic copy` to replicate snapshots between repos
- Could add "replicate to S3" as a post-backup step

---

## New Files

| File | Purpose |
|------|---------|
| `orchestrator/src/routes/backup.rs` | Project backup config + per-project backup endpoints (`/admin/backup/project/`) |
| `orchestrator/src/backup/mod.rs` | Backup manager: scheduling, config resolution, triggering |
| `orchestrator/src/backup/rustic.rs` | Rustic CLI wrapper (init, backup, restore, forget, stats) |
| `orchestrator/src/backup/local.rs` | Local project backup logic (volume export, file collection) |
| `orchestrator/src/backup/scheduler.rs` | Cron scheduler integration |
| `agent/src/routes/backup.rs` | Agent backup endpoints |
| `agent/src/backup/mod.rs` | Agent-side backup logic (same rustic wrapper, local execution) |
| `orchestrator/src/db/migrations/0022_backup_config.sql` | Schema migration |

## Modified Files

| File | Change |
|------|--------|
| `orchestrator/src/routes/mod.rs` | Register backup routes |
| `orchestrator/src/main.rs` | Add backup routes, init scheduler on startup |
| `agent/src/routes/mod.rs` | Register backup routes |
| `agent/src/main.rs` | Add backup routes |
| `orchestrator/src/routes/images.rs` | Save tar to image vault before loading into Docker |
| `orchestrator/src/routes/deploy.rs` | (compose deploy) Save sha256: images to vault on deploy |

---

## Implementation Order

| # | Task | Depends On | Complexity |
|---|------|-----------|------------|
| 1 | DB migration: `backup_config`, `project_backup_overrides`, `image_vault` tables | — | Low |
| 2 | Rustic CLI wrapper (`rustic.rs`) — init, backup, restore, forget, stats, snapshot list | — | Medium |
| 3 | Image vault — save tar on upload, list/restore/delete endpoints | 1 | Low |
| 4 | Agent backup endpoints (`backup.rs`) — init, configure, backup project, restore, forget, status | 1, 2 | Medium |
| 5 | Local backup logic — volume export to temp dir, file collection, rustic backup | 1, 2 | Medium |
| 6 | Orchestrator backup routes — config CRUD, per-project backup/restore, status | 1, 2, 5 | Medium |
| 7 | Backup scheduler — tokio-cron-scheduler, resolve config, trigger backups | 1, 6 | Medium |
| 8 | Push backup config to agents on change | 4, 6 | Low |
| 9 | Dashboard UI — backup settings, per-project status, snapshot list, restore, image vault | 6, 7 | Future |

---

## Verification

1. Enable backup with local storage, trigger backup for a local project with volumes
2. Verify snapshot created in rustic repo, `rustic snapshots` shows correct tags
3. Verify `.env` files are NOT in the backup (`rustic ls {snapshot}`)
4. Upload a sha256: image, verify tar saved to image vault, verify vault entry in DB
5. Trigger backup again, verify only changed chunks are stored (incremental)
6. Run retention policy, verify old snapshots pruned per policy
7. Configure S3 storage, trigger backup, verify snapshots in S3 bucket
8. Restore from snapshot, verify volumes and compose files restored correctly
9. Restore image from vault, verify Docker image loaded, project can redeploy with it
10. Set per-project override with different schedule, verify it takes precedence over global
11. Test with remote project — verify orchestrator triggers agent backup via mTLS
12. Verify backup scheduler runs on cron schedule
