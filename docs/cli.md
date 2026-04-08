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

### Login (session-based)

```bash
l8b login --server https://example.com
```

Prompts for username and password. Saves the session to `~/.config/litebin/session.json`.

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
l8b deploy --project myapp --server https://example.com --port 3000

# CLI flag
l8b deploy --project myapp --server https://example.com --port 3000 --token your-token-here

# Save to config so you don't need to pass it every time
l8b config set --server https://example.com --token your-token-here
l8b deploy --project myapp --port 3000
```

Tokens are created from the dashboard under **Settings > Deploy Tokens**, or auto-generated when using `l8b ship`.

## Commands

### `l8b deploy`

Build and deploy the current directory to a LiteBin project.

```bash
l8b deploy --project <PROJECT_ID> [options]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--project` | *(required)* | Project ID, used as subdomain (`<id>.example.com`) |
| `--port` | `3000` | Internal port the app listens on inside the container |
| `--path` | `.` | Path to the project directory |
| `--dockerfile` | auto-detect | Path to Dockerfile relative to `--path` |
| `--node` | auto-select | Target node ID |
| `--cmd` | image default | Custom command to run in the container |
| `--memory` | server default | Memory limit in MB |
| `--cpu` | server default | CPU limit (e.g. `0.5` for half a core) |
| `--no-auto-stop` | *off* | Disable idle auto-stop |

Global flags (available on all commands):

| Flag | Env | Description |
|------|-----|-------------|
| `--server` | `L8B_SERVER` | Server URL |
| `--token` | `L8B_TOKEN` | Deploy token |

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

### `l8b ship`

Interactive deploy — guided flow for deploying a new or existing project. Requires a prior `l8b login` session.

```bash
l8b ship [options]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--path` | `.` | Path to the project directory |
| `--port` | prompt | Skip port prompt and use this port |

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
  :: Deploying...
  ✔ Deploy successful!

  🌐 Live at: https://my-app.l8b.in

  💡 Use this token to redeploy from CI/CD:
     L8B_TOKEN=a1b2c3d4... l8b deploy --project my-app --port 3000
```

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
4. **Builds, uploads, and deploys** in one step

For advanced options (custom Dockerfile, resource limits, node selection), use `l8b deploy` with flags.

### `l8b login`

Log in to a LiteBin server interactively.

```bash
l8b login --server https://example.com
```

### `l8b logout`

Clear the saved session.

```bash
l8b logout
```

### `l8b config set`

Save configuration so you don't need to pass flags every time.

```bash
# Save server URL
l8b config set --server https://example.com

# Save deploy token
l8b config set --token your-token-here

# Save both
l8b config set --server https://example.com --token your-token-here
```

Config is saved to `~/.config/litebin/config.toml`:

```toml
server = "https://example.com"
token = "your-token-here"
```

### `l8b config show`

Display the current configuration.

```bash
l8b config show
```

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

See [l8bin-action](https://github.com/mtsandeep/l8bin-action) for the reusable composite action:

```yaml
- uses: mtsandeep/l8bin-action@v1
  with:
    server: ${{ secrets.L8B_SERVER }}
    token: ${{ secrets.L8B_TOKEN }}
    project_id: myapp
    port: 3000
```

## Config file location

| Platform | Path |
|----------|------|
| Linux | `~/.config/litebin/config.toml` |
| macOS | `~/Library/Application Support/litebin/config.toml` |
| Windows | `%APPDATA%\litebin\config.toml` |
