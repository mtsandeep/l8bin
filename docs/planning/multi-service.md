# Multi-Service Deployments

LiteBin currently deploys one Docker container per project. This document outlines how to support multi-service (docker-compose style) deployments, and the current workaround using two separate projects.

## Current Workaround: Two Projects (Works Today)

Since all LiteBin containers share the same Docker network (`litebin-network`), containers can already communicate with each other internally by container name.

### How It Works

Deploy two separate projects:

| Project | Image | Container Name | Exposed |
|---|---|---|---|
| `myapp` | Custom Fastify build | `litebin-myapp` | Yes (Caddy route) |
| `myapp-db` | `postgres:16-alpine` | `litebin-myapp-db` | No meaningful HTTP (but Caddy tries) |

The Fastify app connects to Postgres using the internal Docker DNS name:

```
DATABASE_URL=postgres://user:pass@litebin-myapp-db:5432/mydb
```

### Setup Steps

1. Deploy the Postgres project first:
   ```
   POST /deploy
   {
     "project_id": "myapp-db",
     "image": "postgres:16-alpine",
     "port": 5432,
     "cmd": "postgres"
   }
   ```

2. Create `.env` file on the host at `projects/myapp/.env`:
   ```
   DATABASE_URL=postgres://app:secret@litebin-myapp-db:5432/mydb
   ```

3. Deploy the app project:
   ```
   POST /deploy
   {
     "project_id": "myapp",
     "image": "ghcr.io/me/myapp:latest",
     "port": 3000
   }
   ```

### Limitations

- **No env var API** — Must manually create `.env` files on the host filesystem at `projects/<project_id>/.env`
- **No startup ordering** — If Postgres container restarts, the app may fail until it reconnects
- **No shared lifecycle** — Stopping/deleting one project doesn't affect the other
- **No persistence** — When the Postgres container is recreated, all data is lost (no volume support yet)
- **Caddy tries to proxy PG** — Caddy will attempt HTTP reverse proxy to port 5432, which fails harmlessly but is unnecessary noise
- **Resource limits are per-project** — The DB gets its own memory/CPU limits, counted separately against the node
- **Container count** — Each project increments the node's container count independently

### When This Is Good Enough

- Test apps and demos
- Development environments
- Cases where data loss on redeploy is acceptable
- When you need PostgreSQL "now" without platform changes

---

## Planned: Native Multi-Service Support

### Model

```
1 Project → N Services (internal network) → 1 Public Port
```

Only the designated "public" service gets a Caddy route. All other services (databases, caches, workers) are internal-only.

### Example

```
Project: "my-app"
├── web (fastify)     → litebin-my-app-web     → public via Caddy
├── db  (postgres)    → litebin-my-app-db      → internal only
└── Both on per-project network: "litebin-my-app"
       web connects to db:5432 via Docker DNS
```

### Deploy Request Format

```json
{
  "project_id": "my-app",
  "services": {
    "web": {
      "image": "ghcr.io/me/my-app:latest",
      "port": 3000,
      "public": true,
      "depends_on": ["db"]
    },
    "db": {
      "image": "postgres:16-alpine",
      "port": 5432,
      "env": {
        "POSTGRES_USER": "app",
        "POSTGRES_PASSWORD": "secret",
        "POSTGRES_DB": "mydb"
      }
    }
  }
}
```

Backward compatible — single-image deploys still work:

```json
{
  "project_id": "my-app",
  "image": "ghcr.io/me/my-app:latest",
  "port": 3000
}
```

### Changes Required

#### 1. Data Model

New `project_services` table:

```sql
CREATE TABLE project_services (
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service     TEXT NOT NULL,
    image       TEXT NOT NULL,
    port        INTEGER,
    cmd         TEXT,
    env         TEXT,                    -- JSON object
    is_public   INTEGER NOT NULL DEFAULT 0,
    depends_on  TEXT,                    -- comma-separated service names
    container_id TEXT,
    mapped_port  INTEGER,
    PRIMARY KEY (project_id, service)
);
```

The existing `projects` table keeps its high-level fields (`status`, `name`, `description`, `custom_domain`, `node_id`, `auto_stop_*`). Single-image fields (`image`, `internal_port`, `container_id`, `mapped_port`, `cmd`, `memory_limit_mb`, `cpu_limit`) become optional — used only for backward-compatible single-service deploys or as project-level defaults.

#### 2. Docker Manager

Current naming: `litebin-{project_id}`
New naming: `litebin-{project_id}-{service}`

New capabilities needed:

- **Per-project networks** — Create `litebin-{project_id}` bridge network per project. All services join this network. Services resolve each other by service name (e.g., `db:5432`).
- **Volume mounts** — Named volumes for persistent data (e.g., `litebin-{project_id}-db-data` for Postgres). Mount at service-defined paths.
- **DNS aliases** — Services get short aliases (`db`, `redis`) within the per-project network.
- **Health checks** — Optional per-service health check config (for `depends_on` readiness).
- **Startup ordering** — Start services without dependencies first, then start dependents after health checks pass.

The existing `litebin-network` flat network can coexist for backward compatibility. Single-service projects continue using it. Multi-service projects use per-project networks.

#### 3. Deploy Flow

Current flow (single container):

1. Acquire per-project lock
2. Remove old container
3. Pull image
4. `run_container()`
5. Store `container_id`, `mapped_port`
6. Sync Caddy

New flow (multi-service):

1. Acquire per-project lock
2. Remove all old containers for project
3. Pull all images
4. Create per-project network (if not exists)
5. Start services in dependency order (topological sort of `depends_on`)
   - Wait for health checks before starting dependents
6. Store all `container_id` and `mapped_port` values in `project_services`
7. Sync Caddy (route to public service only)
8. On failure: roll back all created containers

#### 4. Caddy Routing

Minimal change. `resolve_routes()` currently produces one upstream per project:

```
{project_id}.{domain} → litebin-{project_id}:{port}
```

With multi-service, route to the public service only:

```
{project_id}.{domain} → litebin-{project_id}-{public_service}:{port}
```

Non-public services have no Caddy entry. Internal communication happens on the per-project Docker network.

#### 5. Agent (Remote Nodes)

The agent's `/containers/run` endpoint needs to accept multi-service definitions. The agent must:

- Create per-project networks
- Start services in dependency order
- Return all container IDs and mapped ports

The agent waker must start all services for a project (in dependency order) instead of a single container.

The agent's `rebuild_local_caddy()` must route to the public service only.

#### 6. Lifecycle Operations

All operations loop over project services:

- **Start** — Start services in dependency order
- **Stop** — Stop in reverse dependency order
- **Recreate** — Remove all, recreate in dependency order
- **Delete** — Remove all containers, per-project network, named volumes
- **Janitor** — Stop all services when project is idle
- **Settings** — Per-service resource limits, env vars, commands

#### 7. Environment Variables

Each service can define its own `env` map. Inter-service references are resolved at deploy time:

```json
{
  "web": {
    "env": {
      "DATABASE_URL": "postgres://app:secret@db:5432/mydb"
    }
  }
}
```

The `db` DNS alias is provided by the per-project Docker network.

#### 8. Volume Persistence

New `project_volumes` concept:

- Named volumes per project (e.g., `litebin-my-app-db-data`)
- Mounted at service-defined paths (e.g., `/var/lib/postgresql/data`)
- Persist across container recreations
- Deleted on project deletion
- The existing stubbed volume export/import routes (`agent/src/routes/volumes.rs`) can be built on top of this

### Migration Strategy

Backward compatible. Existing single-service deploys continue to work unchanged.

1. **Migration 1** — Add `project_services` table. Existing projects get a single row in `project_services` (migrated from `projects` fields).
2. **Migration 2** — Update `Project` struct to load services from the new table. Single-image deploy format creates one service row.
3. **Migration 3** — Add per-project network support to `DockerManager`.
4. **Migration 4** — New multi-service deploy endpoint. Old endpoint continues working.
5. **Migration 5** — Update lifecycle operations for multi-service.

### Implementation Phases

| Phase | Scope | Files |
|---|---|---|
| **Phase 1: Data Model** | New table, migration, updated types | `db/models.rs`, `litebin-common/src/types.rs`, new migration |
| **Phase 2: Docker** | Per-project networks, volumes, naming | `litebin-common/src/docker.rs` |
| **Phase 3: Deploy** | Multi-service deploy + rollback | `orchestrator/src/routes/deploy.rs`, `agent/src/routes/containers.rs` |
| **Phase 4: Lifecycle** | Start/stop/delete/recreate multi-service | `orchestrator/src/routes/manage.rs`, agent counterparts |
| **Phase 5: Routing** | Caddy routes for public service only | `orchestrator/src/routing_helpers.rs`, `agent/src/routes/caddy.rs` |
| **Phase 6: Agent** | Waker + janitor multi-service awareness | `agent/src/routes/waker.rs`, janitor code |
| **Phase 7: Settings + Env** | Per-service config, env var API | `orchestrator/src/routes/settings.rs`, new env endpoint |

### Approximate Effort

| Area | Size | Notes |
|---|---|---|
| DB migration + types | Medium | New table, keep backward compat |
| DockerManager | Medium | Per-project networks, volumes, naming |
| Deploy endpoint | Medium | Services array, ordered startup, rollback |
| Caddy routing | Small | Only public service gets route |
| Agent (remote nodes) | Medium | Mirror deploy + lifecycle changes |
| Start/stop/delete | Small-Medium | Loop over services |
| Waker + Janitor | Small | Multi-container awareness |
| Settings + env API | Small-Medium | Per-service settings, env var API |
| **Total** | ~8-10 files modified, 1 new migration, 1 new table |
