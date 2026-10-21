# LiteBin CLI (`l8b`)

Deploy apps to your LiteBin server from the terminal.

## Install

```bash
curl -fsSL https://l8b.in | bash -s cli
```

## Authentication

The CLI supports two auth methods:

| Method | Use case | Priority |
|--------|----------|----------|
| Deploy token (`L8B_TOKEN`) | CI/CD, scripts, GitHub Actions | 1st |
| Session cookie (`l8b login`) | Local development, interactive use | 2nd |

### Login

```bash
l8b login --server https://l8bin.example.com
```

Opens a login menu: **device pairing** (default — a short code you approve in the dashboard at `/connect`; the CLI's next poll receives a scoped token) or **username/password** (a session cookie). `--pair` skips the menu and runs pairing directly, so agents and scripts can use it headlessly; `--scope` requests a specific token scope. Credentials are saved under `~/.config/litebin/`.

### Logout

```bash
l8b logout
```

Clears the saved session.

### Deploy token

Set the `L8B_TOKEN` environment variable or use `--token`:

```bash
# Environment variable (recommended for CI/CD)
export L8B_TOKEN=your-token-here
l8b deploy --project myapp --server https://l8bin.example.com --port 3000

# CLI flag
l8b deploy --project myapp --server https://l8bin.example.com --port 3000 --token your-token-here

# Save to config so you don't need to pass it every time
l8b config set --server https://l8bin.example.com --token your-token-here
l8b deploy --project myapp --port 3000
```

Tokens are created from the dashboard under **Settings > Access Tokens**, or auto-generated when using `l8b ship`. Each token carries a cumulative scope: `read` (see state), `deploy` (default — also grants read), `manage` (+ lifecycle, settings, env writes), or `admin` (+ delete, nodes, global settings). Tokens can also be bound to a single project.

## Commands

### `l8b deploy`

Build and deploy the current directory to a LiteBin project.

```bash
l8b deploy --project <PROJECT_ID> [options]
```

Common flags: `--port` (internal app port; omitted, the first `EXPOSE` in the Dockerfile is used — port-less web deploys fail fast instead of guessing), `--path`, `--dockerfile`, `--node` (target node), `--cmd`, `--memory`/`--cpu` (resource limits), `--no-auto-stop`, `--background` (no managed URL, always-on), `--compose`/`--service` (multi-service deploys), `--secret` (repeatable, include a specific .env in the build), `--env-file` (extra env file for the build), `--no-env` (exclude build-time env entirely), `--grant-capability` (repeatable: `docker-observe`, `host-network`, `raw-ports`), and `--upload auto|direct|relay`.

> **Every flag, default, and env-var equivalent** (`--server`/`L8B_SERVER`, `--token`/`L8B_TOKEN`, `--ci`/`L8B_CI`, `--json`/`L8B_JSON`, …) is listed in the [CLI Reference](../reference/cli-reference.md) — regenerated from `l8b --generate-markdown` on every release, so it is always current. This page stays narrative on purpose.

### Build strategy

The CLI auto-detects how to build your app:

1. **Dockerfile found** — builds with `docker build`
2. **No Dockerfile** — builds with [Railpack](https://github.com/railwayapp/railpack) (auto-detects Node.js, Python, Go, Rust, etc.)

### Examples

**Basic deploy** (Node.js app on port 3000):

```bash
l8b deploy --project myapp
```

**Python app on port 8080**:

```bash
l8b deploy --project api --port 8080
```

**Custom Dockerfile**:

```bash
l8b deploy --project myapp --dockerfile Dockerfile.prod
```

**Deploy from a different directory**:

```bash
l8b deploy --project myapp --path ./services/frontend
```

**With resource limits**:

```bash
l8b deploy --project myapp --memory 512 --cpu 1.0
```

**Custom container command**:

```bash
l8b deploy --project myapp --cmd "node server.js --production"
```

**Keep running 24/7** (disable auto-stop):

```bash
l8b deploy --project myapp --no-auto-stop
```

**Background project** (worker, agent, or scheduler with no HTTP endpoint):

```bash
l8b deploy --project my-worker --background
```

The same flag applies to Compose deployments. It explicitly sets the project type; Compose services and ports do not infer it:

```bash
l8b deploy --project my-services --compose --background
```

**Compose deploy** (multi-service with docker-compose.yml):

```bash
# Auto-detected when compose file exists
l8b deploy --project myapp

# Selective service rebuild
l8b deploy --project myapp --service api --service worker

# Force compose mode explicitly
l8b deploy --project myapp --compose
```

### `l8b ship`

Interactive deploy — guided flow for deploying a new or existing project. Requires a prior `l8b login` session.

New projects explicitly choose **Web app / HTTP API** (the default) or **Background project**. Background projects skip public service and port prompts and have no managed URL.

```bash
l8b ship [options]
```

Flags (`--path`, `--port` to skip the prompt, `--secret`, `--no-env`): see the [CLI Reference](../reference/cli-reference.md).

#### Flow

```
$ l8b ship
? Deploy to
> New project
  Existing project

? Project name: my-app

  :: Creating project my-app...
  ✔ Project created
  🔐 Deploy token generated for my-app. Save it for CI/CD:
  ! L8B_TOKEN=a1b2c3d4...

? App port [3000]:
  🔍 Detected: Node.js
  :: Building with Docker...
  ✔ Image built
  :: Uploading image...
  ✔ Upload complete
  :: Staging deployment...
  ✔ Deployment staged

  ⏸ Awaiting runtime configuration
     Runtime variables (DB passwords, API keys, etc.) are injected when containers start.
? Runtime env setup
> Push from a local .env — LiteBin writes it for me
  I'll place the .env on the server myself      (shows the node path, waits)
  Skip — this app needs no runtime env

  (after a push the server's env is listed with masked values;
   "Start containers now" is then the default)

  ✔ Deploy successful!

  🌐 Live at: https://my-app.l8b.in

  💡 Use this token to redeploy from CI/CD:
     L8B_TOKEN=a1b2c3d4... l8b deploy --project my-app --port 3000
```

The first deploy moves from `pending` (project created, nothing staged) to `unconfigured` (image/compose and `.env` staged), then pauses so you can edit the server-side `projects/<id>/.env` before any containers start. Choose **Start containers now** immediately if no runtime env is needed; this moves the project through `deploying` to `running`. If you pause, the image stays ready without starting; run `l8b ship` again, select the project, and choose **Resume deployment** (no rebuild).

When selecting **Existing project**, a list of projects is shown with their status, image, and port:

```
? Select project
> my-api        Running    nginx:alpine    port 8080
  web-frontend  Stopped    node:20-alpine   port 3000
  docs-site     Pending    —                —
```

#### What `ship` does automatically

1. **Creates the project** (if new) on the server
2. **Generates a deploy token** scoped to the project — displayed so you can save it for CI/CD later
3. **Detects your framework** (Node.js, Python, Go, Rust, Java, Docker) for informational display
4. **Builds and uploads** images
5. **Stages the first deploy** (writes compose/metadata and creates the target-node `.env`) without starting containers
6. **Waits for runtime configuration**, then starts containers after confirmation

For advanced options (custom Dockerfile, resource limits, node selection), use `l8b deploy` with flags. Non-interactive `l8b deploy` / CI paths do not pause for runtime configuration.

## Bootstrap, health, and server admin

```bash
l8b init                # write l8b.toml project defaults (project, node, server);
                        #   --mcp also emits .mcp.json, --agents adds a managed
                        #   "Deploying (LiteBin)" section to AGENTS.md
l8b doctor              # environment sanity checks with recovery hints
l8b setup --server URL  # first-run bootstrap: create the admin account on a fresh server
l8b mcp                 # run a stdio MCP server exposing l8b as tools (coding agents)
l8b status              # CLI status: stored logins, current server, auth identity
```

Domains and server admin:

```bash
l8b domain set app.example.com      # custom domain for the project (admin)
l8b domain remove                   # detach it again
l8b platform domain set example.com # change the server's platform domain (admin)
l8b platform domain show
```

Backup (see [backup.md](../topics/backup.md)):

```bash
l8b backup status | enable | disable
l8b backup add-s3 | remove-s3
l8b backup restore | revert
```

### `l8b config set`

Save configuration so you don't need to pass flags every time.

```bash
# Save server URL
l8b config set --server https://l8bin.example.com

# Save deploy token
l8b config set --token your-token-here

# Save both
l8b config set --server https://l8bin.example.com --token your-token-here
```

Config is saved to `~/.config/litebin/config.toml`:

```toml
server = "https://l8bin.example.com"
token = "your-token-here"
```

### `l8b config show`

Display the current configuration.

```bash
l8b config show
```

### `l8b cleanup`

Clean up leftover build artifacts (temp generated dockerignore files from interrupted builds).

```bash
l8b cleanup
l8b cleanup --path ./services/frontend
```

## Managing running projects

```bash
l8b list                       # all projects, running-first, with URLs
l8b status --project myapp     # detailed status
l8b status --project myapp --wait --healthy   # exit 0 only when serving 2xx
l8b logs myapp                 # container logs (--tail N, --service S, --deploy)
l8b url myapp                  # the project's managed URL
l8b stop myapp                 # idempotent
l8b start myapp                # polls until running
l8b restart myapp              # recreates containers (applies pending .env changes)
l8b env list myapp             # env keys, masked previews only
l8b env push myapp --file .env.production --apply   # push secrets + apply
l8b domain set myapp app.example.com   # custom domain (also: domain remove)
l8b delete myapp --yes         # removes project, containers, and volumes (admin)
```

### Machine-readable output

Add `--json` (or set `L8B_JSON=1`) to any command for a single JSON object on
stdout with progress suppressed:

```json
{"ok": true, "project_id": "myapp", "status": "running", "url": "https://myapp.example.com"}
```

Failures print `{"ok": false, "error": {"message": "...", "hint": "<recovery command>"}}` and exit 1. `status --wait` and `--healthy` exit non-zero when the project isn't up — designed for scripts and coding agents.

## Configuration priority

Values are resolved in this order (highest priority first):

1. **CLI flags** (`--server`, `--token`)
2. **Environment variables** (`L8B_SERVER`, `L8B_TOKEN`)
3. **Config file** (`~/.config/litebin/config.toml`)
4. **Saved session** (`~/.config/litebin/session.json`)

## Deploy flow

**Interactive** (recommended for first deploy):

```
l8b ship          # guided: create project, generate token, build, deploy
```

**Non-interactive** (CI/CD, scripts):

```
l8b deploy --project myapp --port 3000
```

1. **Build** — Docker build (or Railpack) with image tag `l8b/myapp:latest`
2. **Save** — `docker save` to a temp tar file
3. **Upload** — POST tar to `/images/upload` on the server
4. **Deploy** — POST to `/deploy` on the server, which starts the container

## GitHub Actions

The dashboard can manage the whole thing — `Settings → GitHub` connects a GitHub App (or PAT), and Deploy → GitHub repo writes the workflow + secrets into your repo for you ([github-connect.md](../topics/github-connect.md)).

To wire it by hand, see [l8bin-action](https://github.com/mtsandeep/l8bin-action) for the reusable composite action:

```yaml
- uses: mtsandeep/l8bin-action@v0.2.1
  with:
    server: ${{ secrets.L8B_SERVER }}
    token: ${{ secrets.L8B_TOKEN_MYAPP }}
    project_id: myapp
    # port: 3000               # optional — omit to detect from the Dockerfile EXPOSE
    # compose: true            # multi-service repos
    # services: api,worker     # target services only (compose)
```

The action reports the deployed URL as its `url` output.

## Config file location

| Platform | Path |
|----------|------|
| Linux | `~/.config/litebin/config.toml` |
| macOS | `~/Library/Application Support/litebin/config.toml` |
| Windows | `%APPDATA%\litebin\config.toml` |
