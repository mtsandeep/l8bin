# Disaster Recovery

## Philosophy

LiteBin does not do high availability. If you need HA, use Kubernetes. What LiteBin provides is **fast recovery** — restore a dead master or agent to full operation in minutes from backup.

**What keeps working when the master dies:** All running containers on all agents. Caddy keeps routing traffic. End users see zero downtime.

**What breaks:** Dashboard, deploys, agent commands, DNS changes. Management is unavailable until the master is restored.

**What keeps working when an agent dies:** All containers on other agents. The master and dashboard are unaffected. Only projects on the dead agent are impacted.

---

## Failure Scenarios

| Setup | What Dies | User Impact | Recovery |
|---|---|---|---|
| **Master only** (1 server) | Master → everything down | Full outage | `install.sh master --restore` from S3/R2. Projects, config, all back. |
| **Master + Agent + Cloudflare DNS** (2 servers) | Master dies | **Zero downtime** — agent Caddy serves traffic directly via Cloudflare DNS | `install.sh master --restore` on new VPS. Agent reconnects. |
| **Master + Agent + Cloudflare DNS** (2 servers) | Agent dies | Only that agent's projects go down | New agent + `l8b node recover`. Restore from Rustic. |
| **Master + Agent + Master Proxy** (2 servers) | Master dies | Full outage — traffic routed through master's Caddy | `install.sh master --restore` on new VPS. Agent reconnects. |

**Minimum viable setup:** 1 master (holds projects) + external S3/R2 backup.

**Recommended setup:** 1 master (no projects) + 1 agent (holds projects) + Cloudflare DNS mode + external S3/R2 backup. Master dying doesn't touch user traffic at all. Recovery is purely operational, not urgent.

---

## Prerequisites

This plan depends on two backup systems already planned:

| Backup System | Covers | Planned In |
|---|---|---|
| Litestream (Platform Backup) | Master SQLite DB (project configs, node configs, routes, settings) | Phase 3 (backup.md) |
| Rustic (Project Backup) | Agent project data (Docker volumes, compose files, image vault) | project-backup.md |

Without S3/R2 configured in Litestream, master recovery is limited to local file backup (same server — useless if the server is dead). **S3/R2 is a prerequisite for real DR.**

---

## Master Recovery

### Scenario

Master VPS is dead (hardware failure, provider outage, accidental deletion). Agents are still running. Containers are still serving traffic.

### Recovery Flow

```
1. Spin up a new VPS (or pick an existing agent to promote)
2. ./install.sh master --restore
3. Install guides through S3/R2 config
4. Litestream pulls latest SQLite snapshot from S3/R2
5. Install continues — generates certs, Caddy config, starts stack
6. Orchestrator starts with full DB (all projects, nodes, routes)
7. Reconnect agents (they have stale mTLS certs — need new ones)
8. Full management restored
```

### `install.sh master --restore`

Interactive guided recovery:

```bash
./install.sh master --restore
```

**What it does differently from a fresh install:**

| Step | Fresh Install | `--restore` |
|---|---|---|
| 1. Prompt for domain | Yes | Yes (same) |
| 2. Prompt for routing mode | Yes | Yes (same) |
| 3. Prompt for Cloudflare creds | Yes | Yes (same) |
| 4. Create SQLite DB | Fresh empty DB | **Skip — restore from S3/R2** |
| 5. Prompt for S3/R2 backup config | No | **Yes** (endpoint, bucket, prefix, access key, secret key) |
| 6. Litestream restore | N/A | **Download latest snapshot, place at data/litebin.db** |
| 7. Generate certs | Yes | Yes (same — new CA, new identity) |
| 8. Start stack | Yes | Yes (same) |
| 9. Print next steps | "Add agents" | **"Reconnect agents — they need new certs"** |

### S3/R2 Config Prompts

```
LiteBin Master Recovery
=======================

Restoring from backup. You need your S3/R2 backup credentials.

Backup endpoint: https://<id>.r2.cloudflarestorage.com
Bucket: my-litebin-backups
Prefix [litebin/]:
Access key: ***
Secret key: ***

Connecting to S3/R2...
Found latest snapshot: 2026-05-05T10:30:00Z (2.4 MB)
Restoring database...

Database restored successfully.
  - 12 projects
  - 3 nodes (2 agents + local)
  - 15 routes

⚠  The restored database references 2 agents that will need to reconnect
   with new mTLS certificates. Run the agent reconnect command on each agent.

Starting LiteBin stack...
✓ Orchestrator running
✓ Dashboard running at https://dashboard.l8b.in
✓ Caddy running
```

### DB Patching After Restore

After restoring the SQLite DB on a new server, some state needs fixing:

```sql
-- Local node is now this new server
UPDATE nodes SET name = 'Local', host = 'localhost', public_ip = '',
                   status = 'online', fail_count = 0 WHERE id = 'local';

-- Remote nodes are stale — mark offline, will reconnect
UPDATE nodes SET status = 'offline', fail_count = 0 WHERE id != 'local';

-- Clear stale container references (containers on this new server don't exist yet)
UPDATE projects SET container_id = NULL, mapped_port = NULL
WHERE node_id = 'local' AND status != 'stopped';

-- Invalidate all sessions (different server)
DELETE FROM tower_sessions;
```

This runs automatically during `--restore` before starting the stack.

### Agent Reconnection

After master recovery, agents have stale mTLS certs signed by the old CA. They cannot communicate with the new master. Two options:

**Option A: Re-run agent install (recommended, simplest)**

```bash
# On each agent server
./install.sh agent --reconnect <new-master-ip>
```

This:
1. Stops the agent
2. Fetches new CA cert from the new master
3. Generates new client cert signed by the new CA
4. Updates docker-compose with new master address and certs
5. Starts the agent
6. Agent registers with master, receives project-meta
7. Orchestrator detects which projects are assigned to this node

Projects that were running on the agent before the master died may need redeployment if their containers were stopped or the agent was rebooted. The orchestrator's reconciliation loop handles this — it detects `container_id = NULL` (set during DB patching) and marks the project as needing attention. The user redeploys from the dashboard.

**Option B: Manual cert rotation**

For users who can't re-run install.sh (custom setups), provide a CLI command:

```bash
l8b agent reconnect --master <new-master-ip>
```

Same logic as Option A but via CLI.

### What About `master_proxy` Mode?

In `master_proxy` mode, the master's Caddy proxies traffic to agents. If the master dies, agent traffic stops (Caddy was on the master). After recovery:

1. New master Caddy starts with routes restored from DB
2. Caddy tries to TLS-proxy to agents — agents are still running, traffic flows again
3. If agents were rebooted too, their Caddy instances need the agent to be back online first

In `cloudflare_dns` mode, recovery is simpler — each agent has its own Caddy, DNS points directly to agents. Master recovery doesn't affect live traffic at all.

---

## Agent Recovery

### Scenario

Agent VPS is dead. Master and dashboard are fine. Other agents are unaffected. Only projects on the dead agent are impacted.

### Recovery Flow

```
1. Spin up a new VPS (or reuse an existing one)
2. ./install.sh agent <master-ip>
3. Agent registers with master, appears in dashboard
4. Master assigns the new agent to projects that were on the dead agent
5. Restore project data from Rustic backup
6. User places .env files
7. Redeploy
```

### Step-by-Step

**1. Install new agent**

Normal agent install — same as adding a fresh agent:

```bash
./install.sh agent <master-ip>
```

Agent appears in dashboard as online. No projects assigned yet.

**2. Reassign projects from dead agent**

Dashboard shows the dead agent as "offline" with projects listed. The user selects the new agent and reassigns:

```
Dashboard → Nodes → dead-agent (offline) → "Migrate all projects" → pick new agent
```

This uses the existing migration infrastructure (migration.md Phase 1). The migration flow:

1. Projects marked as "migrating"
2. Project config transferred to new agent (compose.yaml, metadata.json — no .env)
3. **New: restore volumes from Rustic backup** (instead of exporting from dead agent — which is dead)
4. User places .env on new agent
5. Deploy on new agent
6. Cleanup stale DB references to dead agent

**3. Restore from Rustic backup (new step)**

The existing migration plan exports volumes from a live source agent. For dead agents, there's no source to export from — data must come from the Rustic backup.

New flow for dead-agent migration:

```
POST /projects/:id/migrate
{
  "target_node_id": "new-agent-id",
  "source": "backup",           // NEW — restore from backup instead of live agent
  "migrate_volumes": true
}
```

When `source: "backup"`:
1. Skip volume export from source agent (it's dead)
2. Call `POST /internal/backup/project/{id}/restore` on the **target** agent
3. Target agent runs `rustic restore` from its own Rustic repo (same S3/R2 backend)
4. Restored volumes populated on target agent
5. Continue normal deploy flow

**Prerequisite:** The target agent must have Rustic configured with the same S3/R2 repo. This is handled by the orchestrator pushing backup config to agents (already planned in project-backup.md).

### What If Rustic Backup Doesn't Exist?

If the user never configured S3/R2 for Rustic (only local backup), and the agent is dead:

- **Project config is safe** — it's in the master's SQLite DB (compose.yaml stored in `projects/` table or on master filesystem)
- **Docker volumes are lost** — local backup on a dead server is gone
- **Dashboard shows:** "No backup found for this project. Volumes will be empty. You may need to re-import data from your application's own export tools."

The user deploys with empty volumes and re-imports data from their app (e.g., `pg_restore`, database dump, upload files).

### Bulk Agent Recovery

For an agent with many projects, a bulk recovery flow avoids migrating one-by-one:

```
POST /nodes/:id/recover
{
  "target_node_id": "new-agent-id"
}
```

1. List all projects on the dead agent
2. For each project: transfer config, restore volumes from backup, deploy
3. Sequential execution (same as backup — one at a time, avoids RAM/disk contention)
4. Dashboard shows progress
5. When done: remove dead agent from DB, cleanup stale records

---

## CLI Recovery Commands

### Master Recovery

```bash
# Full interactive restore
./install.sh master --restore

# Non-interactive (for scripts)
./install.sh master --restore \
  --s3-endpoint https://<id>.r2.cloudflarestorage.com \
  --s3-bucket my-litebin-backups \
  --s3-prefix litebin/ \
  --s3-access-key $ACCESS_KEY \
  --s3-secret-key $SECRET_KEY
```

### Agent Recovery

```bash
# Fresh agent install (normal)
./install.sh agent <master-ip>

# Then from CLI on any machine with master access:
l8b node recover <dead-agent-id> --target <new-agent-id>

# Or migrate individual projects:
l8b project migrate <project-id> --target <new-agent-id> --source backup
```

---

## Post-Recovery Verification

After any recovery, the dashboard and CLI should show a recovery summary:

```
Recovery Summary
================
Master restored from backup: 2026-05-05T10:30:00Z
  - 12 projects restored
  - 3 nodes (2 offline — need reconnection)

Agent Recovery:
  - node-eu-west: 5 projects restored from backup, 2 redeployed
  - node-us-east: still online, no action needed

⚠  Action required:
  1. Reconnect agent "node-eu-west" — run ./install.sh agent --reconnect on the agent server
  2. Place .env files for projects: myapp, blog-store, api-v2
  3. Redeploy projects: myapp, blog-store (containers not running)
```

---

## What's NOT Covered

| Scenario | Handling |
|---|---|
| Both master AND all agents die | Full restore from S3/R2 (Litestream + Rustic). Install master first, then agents, then restore projects. |
| S3/R2 is also unavailable | No recovery possible. This is why S3/R2 with cross-region replication (e.g., Cloudflare R2 which replicates automatically) is recommended. |
| Certificates/CA lost | Not backed up. New master generates new CA. Agents reconnect with new certs. |
| `.env` files lost | Never backed up (core LiteBin principle). User must recreate from their own records/secrets manager. |
| Docker images lost | Registry images can be re-pulled. `sha256:` uploaded images need the image vault (project-backup.md). |

---

## Implementation Order

| # | Task | Depends On | Complexity |
|---|---|---|---|
| 1 | `install.sh master --restore` — S3/R2 prompts, Litestream restore, DB patching | Litestream S3/R2 (Phase 3) | Low |
| 2 | `install.sh agent --reconnect` — fetch new CA, generate new cert, update config | New master install | Low |
| 3 | Dead-agent migration (`source: "backup"`) — restore volumes from Rustic instead of live agent | Rustic backup (project-backup.md) | Medium |
| 4 | Bulk agent recovery (`POST /nodes/:id/recover`) | 3 | Medium |
| 5 | Post-recovery summary in dashboard | 1, 4 | Low |
| 6 | `l8b node recover` CLI command | 4 | Low |

Items 1-2 are the most impactful (master DR) and lowest effort. Items 3-4 are for the agent scenario and build on the Rustic backup work.
