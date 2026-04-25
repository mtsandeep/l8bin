# Environment & Secret Synchronization

LiteBin distinguishes between **Build-Time Secrets** (baked into your app during image creation) and **Runtime Environment Variables** (injected when your container starts).

## Build-Time vs. Runtime
| Feature | Build-Time (CLI) | Runtime (Agent / Master) |
| :--- | :--- | :--- |
| **Command** | `l8b ship --secret ...` | Handled via server-side project folder |
| **Primary Use** | `NEXT_PUBLIC_*` variables, Static Site Gen | DB URLs, API Keys, Backend salts |
| **Visibility** | Baked into JS/HTML assets | Visible via environment variables |
| **Isolation** | Unique to the specific image build | Can be updated without rebuilding the image |
| **Mechanism** | Merged into `.env` during build | Injected from `projects/<id>/.env` |

---

## Build-Time Secrets (The Build Phase)

LiteBin acts as a secure pre-processor during the `ship` and `deploy` commands. It synchronizes your local environment files to the build context without requiring any modifications to your `Dockerfile` or your `.gitignore`.

### 1. Discovery & Selection
When you run `l8b ship`, the CLI automatically scans your project root for environment files:
- **Automatic Detection**: Finds all files matching the `.env*` pattern.
- **Smart Sorting**: Files are sorted by precedence (base `.env` first, specific `.local` files last).
- **Interactive Multi-select**: The CLI ensures you select at least one file if you choose "Pick specific...".

### 2. Standard Injection
- **Merging**: Selected files are merged into a single temporary `.env` file using a "last-one-wins" rule.
- **Whitelisting**: The CLI automatically ensures this temporary file is included in the Docker build context by using a transient `Dockerfile.dockerignore`.
- **Security**: Since the file is only present during the build and is deleted by the host CLI, your secrets never live in image layers or git history.

---

## Runtime Secrets (Automatic Injection)

For variables that change between environments (like database passwords or internal salts) or that you want to manage manually on the server, LiteBin provides a **Runtime Secret** system.

### 1. Automatic Folder Creation
Upon the first deployment of any project (or any subsequent redeploy), LiteBin's Master or Agent will automatically ensure the following directory structure exists on the host:
```text
litebin/
└── projects/
    └── <project_id>/
        ├── .env          <-- Created automatically (starts as a comment placeholder)
        └── .env.l8bin    <-- Created after first container start (env snapshot)
```

The initial `.env` contains only a comment instructing you to add your runtime variables. You can edit it at any time — LiteBin picks up changes on the next container start.

### 2. Where Runtime Secrets Live

Runtime secrets are stored on the machine that actually runs the container — **not** on the orchestrator.

| Setup | Container runs on | `.env` location | How to edit |
| :--- | :--- | :--- | :--- |
| **Single-node (master only)** | Orchestrator machine | `litebin/projects/<id>/.env` on the master | SSH into master, or edit locally |
| **Multi-node (with agents)** | Agent machine | `litebin/projects/<id>/.env` on the agent | SSH into the agent node |

> **Important:** The orchestrator never directly accesses an agent's filesystem. All env management on agent nodes happens through the agent's own API. If your project is running on an agent node, you must edit the `.env` file on that agent machine.

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
After a successful `ship`, the CLI will display the absolute path to this runtime secret file for easy local management.

---

## Usage Guide

### Interactive Selection
Simply run:
```bash
l8b ship
```
You will be prompted to select which local environment files you'd like to include in the build.

### Explicit Secrets (CLI / CI)
You can also specify secrets manually using the `--secret` flag:
```bash
l8b deploy --port 80 --secret .env --secret .env.production
```

### Best Practices
- **Keep secrets local**: Always keep your active `.env` files in your `.gitignore`.
- **Manage sensitive keys at runtime**: Use the server-side `projects/<id>/.env` for sensitive backend credentials (like `SESSION_SECRET` or `DATABASE_URL`) to keep them out of your image builds entirely.
- **Recreate on change**: If your container is currently running, you can trigger a recreate from the Dashboard to apply env changes immediately. Otherwise, changes are picked up automatically on the next container wake-up.
