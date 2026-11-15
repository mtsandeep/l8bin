# Importing Existing Containers

Adopt Docker containers that are already running on a server into LiteBin — without redeploying them and without downtime. Scan detects non-LiteBin containers (local node or any agent node), groups them, and imports each group as a project: DB rows, managed naming, networking, and routing are set up around the running containers, which are never stopped or recreated. Admin scope; shipped in 0.3.0.

Typical use: you already run things with `docker compose` on a box and want LiteBin to manage them (URLs, auto-stop, logs, env) instead of starting over.

## Scan

`GET /scan` lists foreign containers — anything not named `litebin-*` and not carrying the `litebin.project_id` label. `node_id=local` scans the master's Docker, a specific node id scans that agent, `all` (default) fans out to every online node in parallel. Per-node errors degrade to an empty result, they never fail the request.

Containers are grouped by their Compose project label (`com.docker.compose.project`); containers without one each form their own single-container group. For every group the scan reports:

- the containers (service name, image, state, published ports, volumes — with resolved absolute bind paths)
- whether the original `compose.yaml`/`docker-compose.yml` and a `.env` still exist next to it on that host
- whether images look **local** (built, not from a registry) — importable, but a future redeploy can't re-pull them
- a suggested project id (sanitized group name) and a suggested public service (the container with the lowest internal port that has a host binding)

## Import

`POST /scan/import` adopts selected groups. Per group:

1. **Safety guard** — any bind mount exposing the Docker socket rejects the whole group (redeploy it from a sanitized compose file instead).
2. **Project rows** — a project (status `Importing` → derived once done) and one service row per container, recording the live container ids.
3. **Compose resolution** (compose groups) — the original file is preferred, read from the host (over the agent for remote nodes); if it's gone, a minimal compose is reconstructed from Docker inspect (warning surfaced). Relative bind mounts (`./…`) are rewritten to absolute host paths and `build:` sections are replaced with the running image.
4. **Docker work, in place** — each container is **renamed live** (`litebin-{id}` / `litebin-{id}.{service}`, zero downtime), the managed network `litebin-{id}` is created and attached (existing networks stay attached — nothing is disconnected), and the orchestrator/agent joins the network so it can proxy.
5. **Files — copied, never moved** — LiteBin writes its own `projects/{id}/` with the resolved `compose.yaml` and, if one existed next to the original, its `.env` (a placeholder `.env` otherwise — container env vars are not extracted). The original compose directory is left untouched, and app data stays put: bind mounts keep pointing at their original absolute paths, volumes keep their names.
6. **Routing** — the project gets its `{project_id}.{domain}` subdomain (or a custom domain set afterwards); host port bindings keep working as before.

The review step in the dashboard lets you set the project id, name/description, which service is public, and allow raw host ports before importing.

### What changes vs. what stays

| | |
|---|---|
| Changes | container names, an extra network attachment, LiteBin DB rows + routing + project dir |
| Stays | image, env vars inside containers, original volumes (names and bind paths), other networks, Compose labels, host port bindings |

Two consequences worth knowing: the old `docker compose` project can no longer manage the containers by name (they're renamed out of its convention), and container env vars are **not** extracted — only a `.env` file found next to the original compose travels over, so check env in the dashboard after importing.

## Failures & limits

- **Id collisions** are rejected up front (existing project, or duplicates within one batch).
- **Docker-side failures never fail an import** — they surface as per-project warnings (e.g. one container that couldn't be renamed is skipped and stays foreign; it remains visible on the next scan).
- **No rollback**: DB rows are written before the Docker work; a failure mid-way leaves a partial project behind. Recovery is manual — delete the half-imported project and re-import what's left.
- Stopped containers import fine (nothing is restarted); they show their real state once the periodic Docker reconcile runs (local node).

## Dashboard flow

**Scan & Import** lives at `/manage/import`, reachable from the home page's empty state, the Nodes page header, and a per-node button that pre-filters the scan to that node. Four steps: scan → select groups (socket-exposing groups are blocked from selection) → review ids and options → results with per-project warnings. Groups whose compose file or `.env` was found on disk are marked, so you know whether the stored compose is the original or a reconstruction.

## API (admin scope)

```
GET  /scan?node_id=local|all|{node}   -- foreign container groups per node
POST /scan/import                     -- adopt groups: { groups: [{ node_id, project_id,
                                      --  containers[], deploy_type, public_service?,
                                      --  setup_routing, name?, description? }] }
```

Related: [multi-server.md](multi-server.md) (agents carry scan/import for their node), [multi-service.md](multi-service.md) (the stored compose drives later deploys).
