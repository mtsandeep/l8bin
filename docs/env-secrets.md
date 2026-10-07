# Environment & Secret Synchronization

LiteBin distinguishes between **Build-Time Secrets** (available to your app during image creation) and **Runtime Environment Variables** (injected when your container starts).

## Build-Time vs. Runtime
| Feature | Build-Time (CLI) | Runtime (Agent / Master) |
| :--- | :--- | :--- |
| **Command** | `l8b ship` / `l8b deploy` | Handled via server-side project folder |
| **Primary Use** | `NEXT_PUBLIC_*` variables, Static Site Gen | DB URLs, API Keys, Backend salts |
| **Visibility** | Baked into JS/HTML assets | Visible via environment variables |
| **Isolation** | Unique to the specific image build | Can be updated without rebuilding the image |
| **Mechanism** | Files into the build context (Dockerfile) or Railpack build secrets | Injected from `projects/<id>/.env` |

---

## Build-Time Secrets (The Build Phase)

LiteBin never modifies your local `.env` files — they are read only, exactly as they exist on disk.

### 1. Discovery & Selection
The CLI scans the project root for files matching `.env*` (excluding `.env.example`), sorted by precedence (`.env` first, `.local`/`.prod` last):
- **`l8b ship`** (interactive): shows the detected files and asks **Use these files** (default), **Pick specific files...**, or **Skip build env**. Passing `--secret` or `--no-env` skips the prompt.
- **`l8b deploy`** (non-interactive): includes all detected files automatically. Use `--no-env` to exclude them.

### 2. How Files Reach the Build
- **Dockerfile builds**: the selected files are re-included in the Docker build context via a transient `<Dockerfile>.dockerignore` (removed after the build). Your files themselves are never touched. Note that a `COPY . .` in your Dockerfile **does bake them into image layers** — keep real secrets in runtime env (below).
- **Railpack builds** (no Dockerfile): the selected files are parsed and passed to Railpack as native build-time env. Values are supplied to build steps via BuildKit secret mounts and are **never written to the image or its layers**.

### 3. Multi-Service (compose) Builds
- Services built by Railpack receive the union of root and service-directory env vars.
- Services with their own Dockerfile include only the `.env*` files in that service's build context (root files cannot be included there) — shared variables belong in runtime env.

---

## Runtime Secrets (Automatic Injection)

For variables that change between environments (like database passwords or internal salts) or that you want to manage manually on the server, LiteBin provides a **Runtime Secret** system.

### 0. Env API & CLI (recommended)

Runtime env is managed remotely with write-only values — plaintext is never
returned by the API:

```bash
# list keys (masked previews only)
l8b env list myapp

# push from a file (merge by default; --replace syncs exactly the file)
l8b env push myapp --file .env.production

# push decrypted secrets and apply immediately (recreates the container)
sops -d env.prod | l8b env push myapp --stdin --apply
```

API: `GET /projects/{id}/env` (read scope, masked + `pending_apply` flag) and
`PUT /projects/{id}/env` (manage scope) — see [api-reference.md](api-reference.md).
Storage stays the node-local `projects/<id>/.env` described below; changes
apply on the next container start (recreate).

### 1. Automatic Folder Creation
Upon the **first** deployment of any project, LiteBin stages the project and creates the runtime directory **before containers start**:
```text
litebin/
└── projects/
    └── <project_id>/
        ├── .env          <-- Created automatically (starts as a comment placeholder)
        └── .env.l8bin    <-- Created after first container start (env snapshot)
```

Interactive `l8b ship` moves the project from `pending` to `unconfigured`, then pauses with **Awaiting runtime configuration** so you can configure this file before containers start — push from a local `.env` (LiteBin writes it), place it on the server yourself (the path is shown; LiteBin waits), or skip. If your app needs no runtime env, choose skip/start immediately; the project then moves through `deploying` to `running`. Redeploys offer the same choice in a lighter form — **Skip, no changes to env** is the default (one Enter if you don't want changes) with an optional push that lands before the new containers start.

The initial `.env` contains only a comment instructing you to add your runtime variables. You can edit it at any time — LiteBin picks up changes on the next container start.

### 2. Where Runtime Secrets Live

Runtime secrets are stored on the machine that actually runs the container — **not** on the orchestrator.

| Setup | Container runs on | `.env` location | How to edit |
| :--- | :--- | :--- | :--- |
| **Any setup** | Any node | `litebin/projects/<id>/.env` on the node running the container | `l8b env push` / the env API (proxied over mTLS for agent nodes) |
| **Direct file edit** | Any node | same | Edit the file directly on the node (SSH or local) — same file, same effect |

> **Note:** The orchestrator never directly accesses an agent's filesystem — env writes to agent nodes go through the agent's own mTLS API. `l8b env push` works the same regardless of which node runs the container.

### 3. Manual Management & Injection
You can manually edit the `.env` file inside the project directory on the machine running your container.
- **Auto-Injection**: Every time the container starts (on deploy, recreate, or resume), the Master or Agent reads this file and injects its contents directly into the container's environment.
- **Auto-Detection of Changes**: LiteBin automatically detects when your `.env` has changed since the last container start. If the file has been modified, the container is recreated with the new values on the next wake-up — no manual recreate needed.

### 4. The `.env.l8bin` Snapshot File

Whenever LiteBin injects environment variables into a container, it saves a snapshot of the `.env` file as `.env.l8bin` in the same directory:

```text
litebin/
└── projects/
    └── <project_id>/
        ├── .env          <-- Your runtime secrets (edit this)
        └── .env.l8bin    <-- Auto-generated snapshot (do not edit)
```

**How it works:**
- On container start, LiteBin compares the hash of `.env` with `.env.l8bin`.
- **Same** → Fast path: the existing container is started as-is (instant).
- **Different** → Recreate: a new container is created with the updated env vars.
- After a successful create/recreate, `.env.l8bin` is updated to match the current `.env`.

**What this means for you:**
- Edit `.env` anytime — changes take effect automatically on the next container wake-up.
- You can visually compare `.env` and `.env.l8bin` to see if your changes are live.
- If the files match, your changes are already running. If they differ, they'll be picked up on next start.
- **Do not edit `.env.l8bin`** — it is auto-generated and will be overwritten.

### 5. Usage via CLI
On the first interactive `l8b ship`, the CLI stages the deployment, prints the runtime `.env` path on the selected node, and waits for confirmation before starting containers. If you pause, the image stays ready without starting; run `l8b ship` again, select the project, and choose **Resume deployment** (no rebuild required).

After a successful start, the same path remains available for later secret updates.

---

## Usage Guide

### Interactive Selection
Simply run:
```bash
l8b ship
```
Detected `.env*` files are listed and included with one Enter keypress (**Use these files**), or pick a subset / skip build env entirely.

### Explicit Selection (CLI / CI)
Specify files manually with `--secret` (repeatable), or exclude build env entirely with `--no-env`:
```bash
l8b deploy --port 80 --secret .env --secret .env.production
l8b deploy --no-env
```

### Best Practices
- **Keep secrets local**: Always keep your active `.env` files in your `.gitignore`.
- **Manage sensitive keys at runtime**: Use the server-side `projects/<id>/.env` for sensitive backend credentials (like `SESSION_SECRET` or `DATABASE_URL`) to keep them out of your image builds entirely.
- **Recreate on change**: If your container is currently running, you can trigger a recreate from the Dashboard to apply env changes immediately. Otherwise, changes are picked up automatically on the next container wake-up.
