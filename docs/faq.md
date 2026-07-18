# Frequently Asked Questions & Troubleshooting

## Table of Contents

- [mTLS Certificates](#mtls-certificates)
- [Agent Not Connecting](#agent-not-connecting)
- [Dashboard is Slow / Endpoints Timing Out](#dashboard-is-slow--endpoints-timing-out)
- [Docker Logs](#docker-logs)
- [Networking & Firewalls](#networking--firewalls)
- [Environment Variables (.env)](#environment-variables-env)
- [Docker Compose / Multi-Service](#docker-compose--multi-service)
- [Build Context & .dockerignore](#build-context--dockerignore)
- [Docker Socket Access](#docker-socket-access)
- [Non-HTTP ports and auto-wake](#non-http-ports-and-auto-wake)
- [Volumes & Persistent Data](#volumes--persistent-data)
- [Custom Routes Not Working](#custom-routes-not-working)
- [Docker Images & Disk Space](#docker-images--disk-space)

For a comprehensive view of how LiteBin handles failures at every layer, see [Failure Model](failure-model.md). For why these architectural choices were made, see [Design Decisions](decisions.md).

---

## mTLS Certificates

### How to check if certs were created properly on the master

```bash
# Check host filesystem (run as root)
ls -la /etc/litebin/certs/
# Expected files: ca.pem, server.pem, server-key.pem, agent.pem, agent-key.pem

# Check inside the orchestrator container
docker exec litebin-orchestrator ls -la /certs/
```

### How to check if certs were deployed properly on the agent

```bash
# Check host filesystem
ls -la /etc/litebin/certs/
# Expected files: ca.pem, agent.pem, agent-key.pem

# Check inside the agent container
docker exec litebin-agent ls -la /certs/

# Test the agent health endpoint with mTLS
curl -v --cert /etc/litebin/certs/agent.pem \
  --key /etc/litebin/certs/agent-key.pem \
  --cacert /etc/litebin/certs/ca.pem \
  https://localhost:5083/health
# Expected: JSON response with version, memory_total, cpu_cores, etc.
```

### Cert files and their roles

| File | Used By | Purpose |
|------|---------|---------|
| `ca.pem` | Master + Agent | Root CA that signs both master and agent certs |
| `server.pem` | Master (orchestrator) | Master's TLS certificate (client cert when talking to agents) |
| `server-key.pem` | Master (orchestrator) | Master's private key |
| `agent.pem` | Agent | Agent's TLS certificate (server cert for incoming connections) |
| `agent-key.pem` | Agent | Agent's private key |

### How to show the cert bundle for adding a new agent

```bash
# On the master
curl -fsSL https://l8b.in | bash -s certs --show-bundle
```

### How to regenerate all certs (invalidates all agents)

```bash
# On the master — all agents will need to re-run --update-certs after this
curl -fsSL https://l8b.in | bash -s certs --regenerate
```

### How to update certs on an existing agent

```bash
# On the agent — paste the bundle from `certs --show-bundle` when prompted
curl -fsSL https://l8b.in | bash -s agent --update-certs
```

---

## Agent Not Connecting

### Symptoms

- Agent stuck in `pending_setup` status in the dashboard
- Orchestrator logs show repeated `agent unreachable` warnings
- Node status never changes to `online`

### Step-by-step debug

**1. Is the agent process actually running?**

```bash
# On the agent server
docker ps --filter "name=litebin-agent" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
```

If status is not `Up`, check the logs (see [Docker Logs](#docker-logs) section).

**2. Is the agent listening on the expected port?**

```bash
# On the agent server
ss -tlnp | grep 5083
```

**3. Is the master reachable from the agent?**

```bash
# On the agent server — replace MASTER_IP with your master's public IP
nc -zv MASTER_IP 443 -w 5
```

**4. Is the agent reachable from the master?**

```bash
# On the master server — replace AGENT_IP with your agent's public IP
nc -zv AGENT_IP 5083 -w 5
```

If this times out, see [Networking & Firewalls](#networking--firewalls).

**5. Check orchestrator heartbeat logs**

```bash
docker logs litebin-orchestrator --tail 50 2>&1 | grep -i "heartbeat\|agent unreachable\|pending_setup"
```

---

## Dashboard is Slow / Endpoints Timing Out

### Symptoms

- `/nodes/image-stats` takes a long time to respond
- Node list or other pages load slowly

### Common cause

The orchestrator calls agents sequentially with a 30-minute timeout. If any agent is unreachable, each request blocks for ~30 seconds before timing out.

### Debug

```bash
# Check orchestrator logs for failed agent calls
docker logs litebin-orchestrator --tail 100 2>&1 | grep -i "failed\|timeout\|unreachable"
```

### Fix

Ensure all registered agents are reachable. Remove or decommission unreachable nodes from the dashboard. See [Agent Not Connecting](#agent-not-connecting) to fix the unreachable agent.

---

## Docker Logs

### Container names

| Container | Service |
|-----------|---------|
| `litebin-orchestrator` | Master API server, heartbeat, janitor, project management |
| `litebin-dashboard` | Frontend web UI (Nginx) |
| `litebin-caddy` | Reverse proxy on the master (TLS termination, routing) |
| `litebin-agent` | Worker node daemon (runs on remote servers) |
| `litebin-agent-caddy` | Reverse proxy on the agent (app traffic) |

### Useful log commands

```bash
# Last 100 lines of orchestrator logs
docker logs litebin-orchestrator --tail 100

# Last 100 lines of agent logs
docker logs litebin-agent --tail 100

# Follow orchestrator logs live (Ctrl+C to stop)
docker logs litebin-orchestrator -f

# Filter for heartbeat/connection issues
docker logs litebin-orchestrator --tail 200 2>&1 | grep -i "heartbeat\|connect\|unreachable"

# Filter for agent errors
docker logs litebin-agent --tail 200 2>&1 | grep -i "error\|warn"

# Filter for Caddy routing issues
docker logs litebin-caddy --tail 100 2>&1 | grep -i "error\|502\|503"

# Filter for image-related issues
docker logs litebin-orchestrator --tail 100 2>&1 | grep -i "image"
```

### What orchestrator logs tell you

| Log message | Meaning |
|-------------|---------|
| `heartbeat: node online` | Agent health check succeeded, node is healthy |
| `heartbeat: agent unreachable` | Cannot connect to agent (network/firewall issue) |
| `heartbeat: node marked offline` | 3 consecutive heartbeat failures, node is now offline |
| `heartbeat: attempting to connect pending_setup node` | Trying to push config to a new agent |
| `heartbeat: config pushed to agent` | Agent registration succeeded |
| `failed to get image stats from remote node` | Agent unreachable when fetching image stats |
| `no client for remote node` | Agent was never registered (missing mTLS client in pool) |

---

## Networking & Firewalls

### Required ports

| Port | Protocol | Where | Purpose |
|------|----------|-------|---------|
| 80 | TCP | Master + Agent | HTTP (redirects to HTTPS) |
| 443 | TCP + UDP | Master + Agent | HTTPS (app traffic, QUIC) |
| 5083 | TCP | Agent only | Orchestrator-to-agent mTLS API |

Port 443 on the agent is only needed if using `cloudflare_dns` routing mode. With `master_proxy` (default), only 5083 is needed externally.

### Checking UFW (OS-level firewall)

```bash
sudo ufw status
sudo ufw allow 5083/tcp
```

### Cloud provider firewalls

UFW only controls the OS-level firewall. Most cloud providers (DigitalOcean, Vultr, AWS, GCP, Hetzner) have **separate cloud firewalls** that block traffic before it reaches the VM. You must open port 5083 in both places:

- **DigitalOcean:** Networking → Firewalls → Inbound Rules → Add Custom TCP 5083
- **Vultr:** Settings → Firewall → Edit Firewall Group → Inbound Rules → Add TCP 5083
- **Hetzner:** Firewalls → Add rule → TCP 5083
- **AWS:** Security Groups → Inbound Rules → Custom TCP 5083

### Quick connectivity test

```bash
# From master to agent (replace AGENT_IP)
nc -zv AGENT_IP 5083 -w 5
# Success: "Connection to AGENT_IP 5083 port [tcp/*] succeeded!"
# Failure: "timed out" or "Connection refused"
```

---

## Environment Variables (.env)

### Where do I put runtime environment variables?

Runtime env vars go in `projects/<project_id>/.env` on the machine that runs your container. On a single-node setup, that's the master. On multi-node, it's on the agent where the project is deployed.

```bash
# SSH into your server, then:
echo "DATABASE_URL=postgres://user:pass@db:5432/mydb" >> litebin/projects/myapp/.env
echo "SESSION_SECRET=abc123" >> litebin/projects/myapp/.env
```

LiteBin auto-detects changes to `.env` and recreates the container on the next wake-up with the new values. See [env-secrets.md](env-secrets.md) for the full guide.

### My app doesn't see the env vars I set

1. Make sure you edited `.env` on the correct machine (the one running the container, not the orchestrator for multi-node setups).
2. Check that the file is at `litebin/projects/<project_id>/.env` (not inside the Docker container).
3. The container is recreated automatically on the next wake-up. If it's currently running, trigger a recreate from the dashboard or stop/start the project.
4. Build-time env vars (from `l8b ship --secret .env`) are baked into the image and separate from runtime vars.

### Can I use `${VAR}` in my docker-compose.yml?

Yes. LiteBin supports Docker Compose variable interpolation: `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, `$VAR`, and `$$` (escaped literal). Variables are resolved from the compose `environment` section first, then `.env` files, then system environment.

```yaml
services:
  api:
    image: myapp-api
    environment:
      - DATABASE_URL=${DATABASE_URL:-postgres://localhost:5432/mydb}
      - PORT=${APP_PORT:-3000}
```

---

## Docker Compose / Multi-Service

### How do I deploy a multi-service app?

If a `compose.yaml`, `compose.yml`, `docker-compose.yaml`, or `docker-compose.yml` exists in your project, LiteBin auto-detects it and deploys as multi-service:

```bash
# Interactive (guided)
l8b ship

# Non-interactive (CI/CD)
l8b deploy --project myapp

# Rebuild only specific services
l8b deploy --project myapp --service api --service worker
```

### Only one port is accessible for my service

LiteBin routes traffic to one port per project (the public service's port). Other ports are exposed on the container and accessible for inter-service communication via the Docker network. To expose additional ports externally, enable **Allow raw ports** in the dashboard (Settings → General) — this binds all compose-declared ports directly on the host, including non-HTTP protocols like UDP. See [Non-HTTP ports and auto-wake](#non-http-ports-and-auto-wake) for caveats.

The public service is detected automatically — no labels or config needed:

1. If a service has `litebin.public=true`, it wins (error if multiple have it)
2. Otherwise, a service exposing container port 80 or 443 is picked (if exactly one)
3. Otherwise, if only one service exposes any port, that one is picked
4. Otherwise, the project has no public service (internal-only — useful for databases, workers, etc.)

### LiteBin-reserved ports (even with Allow raw ports)

The host side of your compose `ports:` is always ignored — LiteBin never lets an app choose which host port to bind. When `Allow raw ports` is enabled, LiteBin reads only the *container* port and binds it to the same number on the host (e.g., `ports: - "25565:25565/udp"` works for a Minecraft server).

The following ports are reserved for LiteBin's own services and are **never bound on the host**, even with `Allow raw ports` enabled:

| Port | Used by | Source |
|---|---|---|
| 80 | Caddy HTTP | Fixed (Caddyfile) |
| 443 | Caddy HTTPS / HTTP/3 | Fixed (Caddyfile) |
| 2019 | Caddy admin API | Fixed (Caddyfile) |
| 5080 | Orchestrator HTTP | `PORT` env var, default 5080 |
| 5083 | Agent host port | Conventional install-script mapping |
| 8443 | Agent internal HTTPS | `AGENT_PORT` env var, default 8443 |

If your compose declares one of these (e.g., `ports: - "8282:80/tcp"` for a web app), the binding is silently skipped with a warning log. Web apps on container port 80 or 443 are accessible via their Caddy route (the project subdomain), not via direct host port. Apps that genuinely need raw external access (game servers, non-HTTP databases) should pick a container port outside the reserved list.

### Networks defined in my compose file are ignored

LiteBin creates and manages a single Docker network per project (`litebin_<project_id>`), connects all services and the agent to it, and ignores any `networks` definitions in the compose file. If you have multiple networks for organizing services, they will not be created — all services communicate on the same network.

### My app uses Docker socket (`/var/run/docker.sock`) but it's not working

LiteBin always strips raw Docker socket mounts, including mounts marked read-only. If your app only needs to observe containers:

1. Open the project settings in the dashboard
2. Grant **Docker observation**

LiteBin creates a managed HAProxy sidecar with an endpoint-allowlisted, read-only policy and injects `DOCKER_HOST` only into services that declared the socket. Observation is host-wide: responses may include metadata, environment values, and logs from other projects on that node.

Without `docker-observe`, the raw socket is removed and no replacement is provided. Mutating Docker access is not supported.

### Non-HTTP ports and auto-wake

Auto-wake (sleep on idle, start on request) only works for **HTTP traffic** routed through Caddy. Ports bound directly on the host (via "Allow raw ports") bypass Caddy entirely, so services listening on those ports are **always running** — they won't auto-sleep, and incoming TCP/UDP traffic won't wake a stopped container.

### My compose build context isn't found

Make sure your `build:` directive uses the correct path relative to the compose file. Both forms are supported:

```yaml
# String form
api:
  build: ./api

# Object form (with custom Dockerfile)
api:
  build:
    context: ./api
    dockerfile: Dockerfile.dev
```

---

## Build Context & .dockerignore

### How does `l8b` decide what to exclude from the Docker build context?

The CLI generates a `<Dockerfile>.dockerignore` next to your Dockerfile (Docker auto-detects it via the naming convention). The base content is taken from the **first** of these files that exists **in the project directory** (the folder passed to `--path`, or the current dir):

1. `.dockerignore`
2. `.gitignore` (fallback)

If `.dockerignore` is present, `.gitignore` is **ignored entirely** — they are not merged. This means a stray pattern in `.gitignore` (like a generated folder you need at build time) won't leak into the Docker context once you add a `.dockerignore`.

### My `COPY` fails with "excluded by .dockerignore" for a file that's only in `.gitignore`

This happens when there's no `.dockerignore` in the project dir, so the CLI falls back to `.gitignore` and copies its rules into the generated ignore file. Fix it by adding a `.dockerignore` that lists only what should actually be excluded from the Docker build.

Example — a project where generated docs live in `public/docs/` (gitignored), but the Dockerfile needs them:

```gitignore
# .gitignore
public/docs/
```

```dockerignore
# .dockerignore (now used instead of .gitignore)
node_modules
dist
.git
*.log
```

With `.dockerignore` in place, `public/docs/` is no longer excluded from the Docker context and `COPY public/docs/ public/docs/` works.

### The CLI always creates a `<Dockerfile>.dockerignore` — even when `.dockerignore` exists

Yes. The CLI never reuses `.dockerignore` directly — it always writes a new `<Dockerfile>.dockerignore` whose content is the chosen base (`.dockerignore` if present, else `.gitignore`), optionally followed by `!.env*` when secrets are injected. The original `.dockerignore` is left untouched.

---



### "Permission denied" when my app writes to a volume

Some Docker images run as non-root users (e.g., `USER 1000:1000` or `USER appuser`). When LiteBin creates bind mount directories (`./data`) on the host, Docker creates them as root. The container process then can't write to them.

**How LiteBin handles this:**

LiteBin automatically fixes permissions before starting the container:

1. Inspects the image's `USER` directive via the Docker API
2. For numeric UIDs (`1000:1000`) — runs `chown` on the bind mount directory
3. For string usernames (`appuser`) — falls back to `chmod a+rw` (since the username may not exist on the host)
4. Skips entirely for root users and images with no `USER` directive

This works for both single-service and compose deployments, on **Linux hosts**.

**Windows Docker Desktop limitation:**

Bind mounts on Windows Docker Desktop go through a virtiofs/9p filesystem layer that ignores Linux `chown`/`chmod` calls. If you see "Permission denied" on Windows, add `user: "0:0"` to your compose file:

```yaml
services:
  myapp:
    image: example/app:latest
    user: "0:0"
    volumes:
      - ./data:/app/data
```

**Recommended for production:**

On Linux hosts, the automatic fix works for most images. If your image uses a string username and you want precise ownership instead of the `chmod` fallback, set the numeric `user` in your compose:

```yaml
services:
  myapp:
    image: example/app:latest
    user: "1000:1000"
    volumes:
      - ./data:/app/data
```

### My data disappears when I redeploy

By default, container data is ephemeral. To persist data across recreations and redeployments, add volumes to your compose file:

```yaml
services:
  db:
    image: postgres:16
    volumes:
      - pgdata:/var/lib/postgresql/data

volumes:
  pgdata:
```

LiteBin scopes named volumes to `litebin_<project_id>_<name>`. Bind mounts using relative paths (`./data`) are stored under `projects/<project_id>/data/`. See [volumes.md](volumes.md) for details.

### How do I delete project volumes?

Volumes can be deleted from the dashboard or via the API:

```bash
# Delete a specific volume
curl -X DELETE https://l8bin.example.com/projects/myapp/volumes/pgdata \
  -H "Authorization: Bearer <token>"

# Delete all volumes for a project
curl -X DELETE https://l8bin.example.com/projects/myapp/volumes \
  -H "Authorization: Bearer <token>"
```

---

## Custom Routes Not Working

### My custom route returns 404

1. Ensure the project is running — custom routes are only active for running projects.
2. For path routes: the path is matched on the project's host (subdomain). E.g., `/api` on `myapp.example.com` matches `https://myapp.example.com/api`.
3. For alias routes: the alias must not conflict with other project IDs or existing aliases.
4. After creating a route, Caddy resyncs automatically within ~500ms.

### How do I route to a specific service port?

Set the upstream to the container name and port. For a multi-service project `myapp` with a backend service on port 9090:

```bash
curl -X POST https://l8bin.example.com/projects/myapp/routes \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"route_type": "path", "path": "/api", "upstream": "litebin-myapp.backend:9090"}'
```

---

## Docker Images & Disk Space

### Does LiteBin clean up old Docker images?

Yes. LiteBin automatically removes old images in two scenarios:

- **On redeploy** — the previous image is removed by its sha256 digest after the new container starts successfully. This includes same-tag updates (e.g. redeploying `nginx:latest` when a new version is available). If the image is still used by another project, it is skipped.
- **On project deletion** — all images used by the project's services are removed, not just the public service. Images shared with other running projects are skipped.

### What about images that can't be removed?

If an image is still in use by another container (e.g. two projects share `postgres:16`), it is left in place. LiteBin only removes images that are no longer referenced by any container.

### What are dangling images?

Dangling images are untagged images left behind by Docker when a tag is reassigned to a newer version. LiteBin's digest-based cleanup prevents this from happening during normal operations. You can also manually prune any remaining dangling images from the dashboard (Nodes → Prune Images).

### My disk is full of Docker images

LiteBin cleans up automatically on redeploy and delete. If you still need to reclaim space:

```bash
# Prune all dangling images on the host
docker image prune

# Prune ALL unused images (not just dangling)
docker image prune -a
```
