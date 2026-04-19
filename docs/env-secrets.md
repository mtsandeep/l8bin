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
        └── .env  <-- Placeholder created automatically
```

### 2. Manual Management & Injection
You can manually edit the `.env` file inside the project directory on your server.
- **Auto-Injection**: Every time the container starts (on deploy, recreate, or resume), LiteBin reads this file and injects its contents directly into the container's environment.
- **Refresh Required**: Because Docker environment variables are set at container creation, you must **Reploy** or **Recreate** the project via the CLI/Dashboard after manually updating the server-side `.env`.

### 3. Usage via CLI
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
- **Recreate on change**: Always remember to run `l8b ship` (Redeploy) if you manually update a server-side secret.
