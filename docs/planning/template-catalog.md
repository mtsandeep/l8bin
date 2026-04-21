# Template Catalog

A template system for one-click project deployment. Users pick a template, fill in a few inputs, and get a running multi-service or single-service project.

**Status:** Post-MVP

---

## Core vs Add-on

Templates have two layers. The core is built into LiteBin. The add-ons are optional paid services.

| Layer | What | Required? | Runs Where |
|---|---|---|---|
| **Built-in templates** | Pre-built image templates bundled with LiteBin (postgres, redis, etc.) | Always available | User's VPS |
| **Remote catalog** | Curated template library served from a URL | No | External (static file host) |
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

Templates are directories with a compose file and a LiteBin manifest:

```
templates/
├── ecommerce/
│   ├── template.yml              # LiteBin manifest (prompts, routes, public service)
│   ├── docker-compose.yml        # Service definitions
│   └── .env.example              # Hint for required env vars
├── postgres/
│   ├── template.yml
│   └── docker-compose.yml        # Single-service template
├── nextjs/
│   ├── template.yml
│   ├── Dockerfile                # Single-service with Dockerfile
│   └── .env.example
└── wordpress/
    ├── template.yml
    ├── docker-compose.yml
    └── .env.example
```

### Two Template Types

| Type | Has | Build Required | Example |
|---|---|---|---|
| Pre-built image | `docker-compose.yml` with image references | No | PostgreSQL, Redis |
| Dockerfile | `Dockerfile` (+ optional `docker-compose.yml`) | Yes | Next.js app |

Dockerfile templates require a build server. Pre-built image templates deploy directly.

---

## Template Manifest (template.yml)

```yaml
name: E-commerce Stack
description: Next.js storefront + PostgreSQL + Redis
version: 1.0

# Which service is public (gets Caddy route)
public: web

# Prompts shown to user during deploy
prompts:
  - name: STORE_NAME
    label: "Store name"
    default: "my-store"
    target: env

  - name: ADMIN_EMAIL
    label: "Admin email"
    required: true
    target: env

  - name: DB_PASSWORD
    label: "Database password"
    generate: password
    target: env

  - name: PROJECT_SUBDOMAIN
    label: "Project ID"
    default: "my-store"
    target: project_id

# Routes created automatically on deploy
routes:
  - type: path
    path: "/admin/*"
    service: admin
    description: "Admin panel"
```

### Prompt Types

| Type | Description | Example |
|---|---|---|
| `target: env` | Value injected into project `.env` | `STORE_NAME=my-store` |
| `target: project_id` | Value used as the project ID | `my-store` |
| `generate: password` | Auto-generate a secure random value | `xK9mR2pL4vN7` |

### Variable Resolution

Compose files use `${VAR}` syntax. Resolved from user prompts first, then project `.env`:

```yaml
# docker-compose.yml (in template)
services:
  db:
    environment:
      POSTGRES_PASSWORD: ${DB_PASSWORD}    # ← from user prompt
      POSTGRES_DB: ${STORE_NAME}           # ← from user prompt
```

---

## Deploy Flow

```
1. User selects template (dashboard or CLI)
2. LiteBin reads template.yml → shows prompts
3. User fills inputs (name, email, password, etc.)
4. LiteBin generates project .env from user inputs
5. LiteBin reads docker-compose.yml → resolves ${VAR} from .env
6. Injects litebin.public: "true" label on the service specified in template.yml
7. Template needs building (has Dockerfile)?
   ├─ No  → skip to step 8
   └─ Yes → build server configured?
        ├─ Yes → send template + inputs + project deploy token + VPS address to build server
        │        (authenticated with L8BIN_BUILD_TOKEN)
        │        build server builds image → tar → sends to VPS with project deploy token
        │        VPS loads image (docker load) → image available locally
        └─ No  → skip Dockerfile templates (pre-built templates still available)
8. Validate via compose-bollard (topological sort, cycle detection)
9. Store resolved compose.yml at projects/{id}/compose.yml
10. Deploy (same multi-service deploy flow)
11. Create routes from template.yml manifest
12. Project is running
```

After deploy, the project is a normal multi-service project. The stored `compose.yml` is the source of truth — the template is not referenced again. Users can freely modify the compose file.

### LiteBin Overrides

Template compose files get the same 4 overrides as any compose deploy (binds, port_bindings, networking_config, env). The template's `ports`, `volumes`, and `networks` are informational — LiteBin controls these.

---

## Build Server (Add-on)

An optional service for building Dockerfile-based templates. LiteBin never builds images on the user's VPS — a build server handles it. Without it, only pre-built image templates are available from the catalog. All other LiteBin features work normally.

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
     Payload: template, user inputs, user's project deploy token, VPS node address

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

Bundled with LiteBin:

| Template | Services | Description |
|---|---|---|
| `postgres` | PostgreSQL 16 | Standalone database |
| `mysql` | MySQL 8 | Standalone database |
| `redis` | Redis 7 | Standalone cache |
| `mongodb` | MongoDB 7 | Standalone database |
| `wordpress` | WordPress + MySQL | Blog/CMS |
| `nextjs-postgres` | Next.js + PostgreSQL | Full-stack web app |

Each includes sensible defaults (shm_size for postgres, healthchecks, resource limits).

---

## API

```
GET  /templates                  — List all templates (name + description)
GET  /templates/:name            — Get template details + prompts
POST /projects/deploy            — Deploy from template (new format)
```

Deploy from template:

```json
POST /projects/deploy
{
  "project_id": "my-store",
  "template": "ecommerce",
  "inputs": {
    "STORE_NAME": "my-store",
    "ADMIN_EMAIL": "admin@example.com",
    "DB_PASSWORD": "xK9mR2pL4vN7"
  }
}
```

---

## CLI

```
l8b deploy --template ecommerce my-store
# → prompts for ADMIN_EMAIL, auto-generates DB_PASSWORD
# → deploys full stack

l8b deploy --template postgres my-db
# → prompts for DB_PASSWORD
# → deploys standalone postgres

l8b template list
l8b template info ecommerce
```

---

## Dashboard

Template picker on the deploy page:
1. Grid of template cards (name, description, service count)
2. Click template → modal with prompts
3. Fill inputs → deploy
4. Redirect to project page

---

## Custom Templates (Future)

Users define their own templates outside the built-in set:

```
# Local directory
l8b deploy --template ./my-template my-project

# Git repo (future)
l8b deploy --template github.com/user/litebin-templates/nextjs my-project
```

Same structure required: `template.yml` + compose file.

---

## Remote Catalog (Add-on)

An optional curated template library served from a remote URL. Adds more templates beyond the built-in set. Without it, built-in templates work as normal — nothing changes.

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
GET {url}/templates/{name}/template.yml    → manifest
GET {url}/templates/{name}/docker-compose.yml
GET {url}/templates/{name}/.env.example
```

**index.json** at the catalog root:

```json
[
  { "name": "ecommerce", "description": "E-commerce Stack", "version": "1.0" },
  { "name": "saas-starter", "description": "SaaS boilerplate with Stripe", "version": "2.1" }
]
```

### Merged Template List

Built-in templates and remote catalog templates appear as one list to the user. Remote templates override built-in ones with the same name.

```
$ l8b template list

Built-in:
  postgres       PostgreSQL 16
  redis          Redis 7

Remote (catalog.litebin.in):
  ecommerce      E-commerce Stack
  saas-starter   SaaS boilerplate with Stripe
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
| compose-bollard | Template compose files parsed by compose-bollard |
| Custom Routes (pre-MVP) | Template routes created via same `project_routes` CRUD |
| Multi-service deploy | Same deploy flow — compose file comes from a template |

---

## Files Modified

| File | Change |
|---|---|
| `litebin-common/src/templates.rs` | Template manifest parsing, prompt types, variable resolution |
| `litebin-common/src/templates/` | Built-in template files |
| `orchestrator/src/routes/templates.rs` | Template list + info endpoints |
| `orchestrator/src/routes/deploy.rs` | Template deploy format (template + inputs) |
| Dashboard | Template picker UI |

---

## Implementation Scope

### Scope

- Template manifest format (template.yml)
- Built-in templates: postgres, mysql, redis, mongodb (pre-built images, no build needed)
- Prompt types: env, project_id, password generation
- Variable resolution in compose files
- Build server integration (config, build flow)
- Remote catalog (config, fetch, cache)
- CLI: `--template` flag on deploy
- Dashboard: template picker
