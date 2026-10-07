# Platform Backup (SQLite)

LiteBin's platform data — the SQLite database containing all project configs, node configs, routes, and settings — is backed up via [Litestream](https://litestream.io/).

**Status: shipped.** Backup is opt-in from the dashboard (Settings → Backup) or CLI. No custom backup code — LiteBin generates the config and manages the sidecar container.

## How It Works

```
┌─────────────────────────────┐      ┌──────────────────────────────┐
│  litebin-orchestrator       │      │  litebin-litestream (sidecar)│
│                             │      │                              │
│  /app/data/litebin.db  ◄────┼──────┼─  /data/litebin.db (ro use)  │
│  /app/data/litestream.yml ──┼─────►│  /data/litestream.yml (cfg)  │
│                             │      │         │ WAL streaming      │
│  owns config, manages       │      │         ▼                    │
│  container via Docker API   │      │  /data/backups/litebin.db    │
│                             │      │  (local replica)             │
│                             │      │  s3://bucket/prefix (option) │
└─────────────────────────────┘      └──────────────────────────────┘
          both mount the same `orchestrator-data` volume
```

- **Opt-in from the dashboard.** Nothing is installed by default. "Enable backup" pulls `litestream/litestream:0.5.17`, creates the sidecar container via the Docker API (same mechanism as project containers), and starts streaming. Disabling stops and removes the container; replicas stay on the volume and resume on re-enable.
- **The orchestrator owns `litestream.yml`** (at `/app/data/litestream.yml`, shared via the volume). Generated from DB `settings` rows at boot and on every config change. The DB is the single source of truth — `install.sh` never writes it, and updates never clobber user config.
- **Local replica is the default destination** (`file:///data/backups/litebin.db`, 30-day retention). **S3/R2 is an additional destination**, not an alternative — once configured, Litestream streams to both simultaneously.
- **Master-only.** Agents hold no platform state.

## Settings Keys

Stored in the `settings` table: `backup_enabled`, `backup_s3_endpoint`, `backup_s3_bucket`, `backup_s3_prefix`, `backup_s3_access_key`, `backup_s3_secret_key`. Secrets are stored plaintext — same as `cloudflare_api_token`. Encryption at rest is S3 SSE (bucket-side), not LiteBin's job.

## API (admin scope)

```
GET    /backup/status    → { enabled, available, sidecar_installed, sidecar_running, last_sync, s3: { configured, endpoint, bucket, prefix } }
POST   /backup/enable    → create + start sidecar (first enable pulls the image, ~30s)
POST   /backup/disable   → stop + remove sidecar (replicas kept)
POST   /backup/s3        → { endpoint, bucket, prefix?, access_key_id, secret_access_key } — regenerates yml, restarts sidecar
DELETE /backup/s3        → clear S3 config, regenerate yml, restart sidecar
POST   /backup/restore   → { timestamp? } — point-in-time (RFC 3339) or latest
```

## Restore

The orchestrator runs *from* the database it would restore, so restore is a two-phase swap:

```
1. POST /backup/restore execs in the sidecar:
     litestream restore -config /data/litestream.yml -o /data/litebin.db.restored -force [-timestamp T]
2. On success the orchestrator responds 200 and exits (restart: unless-stopped revives it)
3. At boot — before the DB pool opens — litebin.db.restored is swapped in:
     old db/-wal/-shm → litebin.db.pre-restore* (one-shot safety copy)
     litebin.db.restored → litebin.db
```

The dashboard polls `/backup/status` until the orchestrator is back. Stale `-wal`/`-shm` files are deleted with the swap — the restored file is a plain snapshot.

## CLI

```
l8b backup status
l8b backup enable
l8b backup disable --yes
l8b backup add-s3 --endpoint https://<id>.r2.cloudflarestorage.com --bucket my-backups \
                  --access-key AKIA... --secret-key ...
l8b backup remove-s3 --yes
l8b backup restore [--timestamp 2026-10-06T10:30:00Z] --yes
```

## Resource Impact

| Component | RAM |
|-----------|-----|
| LiteBin idle | ~33 MB |
| Litestream (only when enabled) | ~5-10 MB |

Zero containers and zero RAM for installs that never enable backup.

## Implementation

| Piece | Location |
|---|---|
| Sidecar lifecycle, exec, restart, mount detection | `litebin-common/src/docker/container/sidecar.rs` |
| yml generation, restore swap, replica status, boot reconcile | `orchestrator/src/backup.rs` |
| HTTP endpoints | `orchestrator/src/routes/backup.rs` |
| Dashboard Backup tab | `dashboard/src/components/GlobalSettingsModal.tsx` |
| CLI | `cli/src/commands/backup.rs` |
| Tests | `orchestrator/src/backup.rs` (unit), `orchestrator/src/tests/backup_tests.rs` (HTTP) |

`install.sh` is unchanged — the sidecar exists only after the user enables backup.

## Not Covered Here

Project data (Docker volumes, uploaded images, compose files) is a separate plan: [project-backup.md](project-backup.md). Disaster recovery (dead-master restore from S3) builds on this: [disaster-recovery.md](disaster-recovery.md).
