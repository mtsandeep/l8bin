# Template Catalog

A template system for one-click project deployment. Users pick a template (or paste any compose file), fill in env vars, and get a running multi-service or single-service project.

**Philosophy:** No vendor lock-in. A template is just a standard docker-compose.yml — the same file you'd use with `docker compose up` anywhere. Bring your own compose files from Docker Hub, GitHub, or any self-hosted community. LiteBin parses it in the browser to auto-generate a setup form for env vars and image tags, then deploys it as-is. Your compose file stays untouched; your secrets stay in your browser and are never sent through the server.

**Status:** Post-MVP

---

## Core vs Add-on

Templates have two layers. The core is built into LiteBin. The add-ons are optional paid services.

| Layer | What | Required? | Runs Where |
|---|---|---|---|
| **Built-in templates** | Pre-built image compose files bundled with LiteBin (postgres, redis, etc.) | Always available | User's VPS |
| **Remote catalog** | Curated compose library served from a URL | No | External (static file host) |
| **Build server** | Builds Dockerfile-based templates | No | External (managed by LiteBin) |

Without catalog and build server, users still have:
- All built-in templates (pre-built images, deploy directly)
- Direct compose file upload
- Direct image deploy (existing single-service flow)

Core LiteBin features are completely unaffected by the absence of catalog or build server. Nothing is gated, nothing feels broken.

---

## Why

Deploying a common stack (blog + database, e-commerce + postgres + redis) currently requires writing a compose file, setting up env vars, configuring routes, then deploying. Templates eliminate this — pick a stack, answer a few questions, deploy.

---

## Template Structure

### No Custom Manifest — Plain Docker Compose

Templates are **standard docker-compose.yml files**. No LiteBin-specific manifest, no `template.yml`, no custom format. Any valid compose file from any source works as a template.

```
templates/
├── wordpress/
│   └── docker-compose.yml
├── postgres/
│   └── docker-compose.yml
├── wordpress-mysql/
│   └── docker-compose.yml
└── n8n/
    └── docker-compose.yml
```

### Two Template Types

| Type | Has | Build Required | Example |
|---|---|---|---|
| Pre-built image | `docker-compose.yml` with image references | No | PostgreSQL, Redis, WordPress |
| Dockerfile | `Dockerfile` (+ optional `docker-compose.yml`) | Yes | Next.js app |

Dockerfile templates require a build server. Pre-built image templates deploy directly.

---

## Compose Parsing & Env Handling

All compose parsing happens in the **frontend** (dashboard browser). No server-side parsing needed.

### What the parser extracts

From any docker-compose.yml:

1. **Services** — list of services, user can toggle which to deploy
2. **Images** — `image:tag` for each service (editable, e.g., pin a version)
3. **Ports** — first port mapping per service (detects internal port for LiteBin routing)
4. **Environment variables** — all env vars from `services.*.environment`
   - Empty values → required fields (highlighted)
   - Keys containing PASSWORD/SECRET/TOKEN → password input fields
   - Values referencing other services (e.g., `db`) → auto-filled, read-only
   - Everything else → text inputs with current value as default

No special library needed — just a YAML parser (`js-yaml`) + walking the object. ~100-150 lines of TypeScript.

### Env vars stay in browser

LiteBin's core principle: `.env` files are never sent through the server. For templates:

1. Compose.yml is sent to the deploy endpoint **with env var values replaced by `${VAR_NAME}` references** — no secrets in the compose
2. The `.env` content is generated in the browser from user's form inputs
3. Dashboard shows a **read-only `.env` preview** during template creation — user copies it manually to the project's `.env` on the agent
4. `.env` never touches the orchestrator, never travels over the network

```
Template compose.yml:
  MYSQL_ROOT_PASSWORD: changeme
       ↓ frontend transforms
Deployed compose.yml:
  MYSQL_ROOT_PASSWORD: ${MYSQL_ROOT_PASSWORD}

.env (generated in browser, user copies to agent):
  MYSQL_ROOT_PASSWORD=mySecurePass123
```

### Template sources

| Source | How | Example |
|---|---|---|
| Bundled catalog | Static compose files in dashboard image | WordPress, PostgreSQL |
| Paste | User pastes compose.yml into dashboard | Any compose from the internet |
| Upload | User uploads a compose file | Local compose files |
| URL | Dashboard fetches from a URL (future) | GitHub raw links |

All sources go through the same parser. No LiteBin-specific format needed.

---

## Deploy Flow

```
1. User selects template or pastes/uploads compose.yml (dashboard or CLI)
2. Frontend parses compose.yml → extracts services, images, ports, env vars
3. Dashboard shows auto-generated form (env vars, image tags, service toggles)
4. User fills in values
5. Frontend transforms compose.yml: hardcoded env values → ${VAR_NAME} references
6. Frontend generates .env content from user inputs (stays in browser)
7. Dashboard shows .env preview — user copies to project's .env on agent
8. Deployed compose.yml (with ${VAR} refs) sent to orchestrator deploy endpoint
9. Validate via compose-bollard (topological sort, cycle detection)
10. Store compose.yml at projects/{id}/compose.yml
11. Deploy (same multi-service deploy flow)
12. Project is running
```

After deploy, the project is a normal multi-service project. The stored `compose.yml` is the source of truth — the template is not referenced again. Users can freely modify the compose file.

### LiteBin Overrides

Template compose files get the same 4 overrides as any compose deploy (binds, port_bindings, networking_config, env). The template's `ports`, `volumes`, and `networks` are informational — LiteBin controls these.

---

## Build Server (Future Add-on)

An optional service for building Dockerfile-based templates. This is a **future feature** — the initial catalog release only supports pre-built image templates. LiteBin never builds images on the user's VPS — a build server handles it. Without it, only pre-built image templates are available from the catalog. All other LiteBin features work normally.

### Config

```toml
[build]
server = ""                   # build server URL (empty = no build server)
token = ""                    # authenticates LiteBin → build server
```

Or via env:

```
L8BIN_BUILD_SERVER=https://build.litebin.in
L8BIN_BUILD_TOKEN=build_auth_xxx
```

### Build Flow

The build server uses the same mechanism as `l8b ship`:

```
LiteBin → Build Server:
  1. Sends build request with L8BIN_BUILD_TOKEN (authenticates with build server)
     Payload: compose file, user's project deploy token, VPS node address

Build Server:
  2. Builds Docker image (docker build)
  3. Tars the image (docker save)

Build Server → User's VPS:
  4. Sends tar to user's VPS via API (authenticated with project deploy token)

User's VPS:
  5. Receives tar → loads image (docker load)
  6. Image available locally → deploy proceeds
```

### No Build Server

If a template needs building and no build server is configured, Dockerfile-based templates are not available. This does not affect any other functionality. Pre-built image templates and direct compose upload work as normal.

Users who want Dockerfile-based templates can:
1. Build locally with `l8b ship` and push to a registry, then deploy from image
2. Use GitHub Actions to build and deploy
3. Configure a build server to unlock Dockerfile-based templates from the catalog

---

## Built-in Templates

Bundled with LiteBin as plain docker-compose.yml files. Curated from popular self-hosted apps (reference: CapRover, Dokploy, Coolify open-source catalogs).

| Template | Services | Description |
|---|---|---|
| `postgres` | PostgreSQL 16 | Standalone database |
| `mysql` | MySQL 8 | Standalone database |
| `redis` | Redis 7 | Standalone cache |
| `mongodb` | MongoDB 7 | Standalone database |
| `wordpress` | WordPress + MySQL | Blog/CMS |
| `nextcloud` | Nextcloud | File sync and sharing |
| `n8n` | n8n | Workflow automation |
| `uptime-kuma` | Uptime Kuma | Status monitoring |
| `grafana` | Grafana + Prometheus | Metrics dashboard |

Each includes sensible defaults (healthchecks, resource limits where applicable).

---

## API

No template-specific endpoints needed. The existing compose deploy flow handles everything:

```
POST /projects/:id/deploy         — Deploy compose.yml (same as manual compose deploy)
```

The dashboard handles compose parsing and form generation client-side. The orchestrator receives a standard compose.yml with `${VAR}` references — no template awareness needed on the backend.

---

## CLI

```
l8b deploy --template wordpress my-blog
# → shows parsed env vars from compose, prompts for values
# → generates .env preview for user to copy
# → deploys

l8b deploy --compose ./my-stack.yml my-project
# → same flow, just from a local file instead of bundled template

l8b template list
l8b template info wordpress
```

---

## Dashboard

Template picker on the deploy page:
1. Grid of template cards (name, description, service count)
2. Click template → modal with auto-generated form (env vars, image tags)
3. Fill values → see .env preview (read-only, copy button)
4. Copy .env to agent → deploy
5. Redirect to project page

Users can also paste/upload any compose.yml directly — same form, same flow.

---

## Custom Templates (Future)

Users define their own templates outside the built-in set:

```
# Local file
l8b deploy --compose ./my-stack.yml my-project

# URL (future)
l8b deploy --compose https://raw.githubusercontent.com/user/stack/main/compose.yml my-project
```

Any valid docker-compose.yml works. No special structure required.

---

## Remote Catalog (Add-on)

An optional curated compose library served from a remote URL. Adds more templates beyond the built-in set. Without it, built-in templates work as normal — nothing changes.

### Config

```toml
[catalog]
url = ""                                  # remote catalog base URL (empty = no remote templates)
token = ""                                # optional — sent as Authorization header
```

Or via env:

```
L8BIN_CATALOG_URL=https://catalog.litebin.in
L8BIN_CATALOG_TOKEN=Bearer ...
```

When `url` is empty (default), only built-in templates are available. No remote fetching, no network needed.

### How It Works

The catalog URL serves static files — no server logic needed (S3, Cloudflare Pages, GitHub Pages, any static host).

```
GET {url}/index.json                       → list of available templates
GET {url}/templates/{name}/docker-compose.yml
```

**index.json** at the catalog root:

```json
[
  { "name": "wordpress", "description": "Blog/CMS with MySQL", "services": 2 },
  { "name": "nextcloud", "description": "File sync and sharing", "services": 1 }
]
```

No custom manifest files — just compose.yml files. The frontend parses them identically to bundled templates.

### Merged Template List

Built-in templates and remote catalog templates appear as one list to the user. Remote templates override built-in ones with the same name.

```
$ l8b template list

Built-in:
  postgres       PostgreSQL 16
  redis          Redis 7

Remote (catalog.litebin.in):
  wordpress      Blog/CMS with MySQL
  nextcloud      File sync and sharing
```

When no catalog URL is configured, only built-in templates are shown.

### Caching

Fetched templates are cached locally. Cache duration is configurable (default: 1 hour). Force refresh with `l8b template list --refresh`.

### Auth

If the catalog URL requires authentication, set the `token` config. LiteBin sends it as an `Authorization` header on every request. If the catalog returns 401/403, LiteBin shows "catalog access denied" and falls back to built-in templates only.

---

## Relationship to Other Components

| Component | Relationship |
|---|---|
| compose-bollard | Template compose files parsed by compose-bollard (same as manual compose) |
| Custom Routes | Routes created via same `project_routes` CRUD (same as manual compose) |
| Multi-service deploy | Same deploy flow — compose file comes from a template instead of user upload |
| Per-project .env | `.env` generated in browser, user copies to agent (never over network) |

---

## Files Modified

| File | Change |
|---|---|
| `dashboard/src/` | Compose parser, auto-form generator, .env preview, template picker UI |
| `dashboard/static/templates/` | Bundled docker-compose.yml files |
| `cli/src/` | `--template` and `--compose` flags, compose parser for CLI |
| `orchestrator/src/routes/deploy.rs` | No changes needed — receives standard compose.yml |

No changes to `litebin-common` or the orchestrator — all template logic is frontend-only.

---

## Implementation Scope

### Scope

- Compose parser (frontend): extract services, images, ports, env vars
- Auto-form generator: env var fields, image tag editing, service toggles
- `.env` generator: extracts env vars from compose, pre-fills defaults, user fills in values, generates .env file to copy to agent (future: swap manual copy with secret storage once implemented)
- Built-in templates: postgres, mysql, redis, mongodb, wordpress, n8n, uptime-kuma, grafana, nextcloud
- Template sources: bundled, paste, upload
- Build server integration (config, build flow)
- Remote catalog (config, fetch, cache)
- CLI: `--template` and `--compose` flags
