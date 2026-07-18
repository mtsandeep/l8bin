# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- **Compose compatibility report** — Deploy validates Compose files and reports supported, translated, overridden, permission-required, and unsupported fields instead of silently ignoring them.
- **Project capabilities** — Explicit grants (`docker-access`, `raw-ports`) replace ad-hoc permission toggles. Compose deploy can request them; users approve in CLI/`l8b deploy --grant-capability` or the dashboard validation step. Existing Compose projects manage grants under Settings → Capabilities.

### Fixed
- **Dashboard log viewer strips ANSI** — Container logs with terminal color codes render as plain text instead of raw escape sequences like `\u001b[95m`.
- **`/compose/validate` proxied to orchestrator** — Added `/compose/*` to Caddy API path matchers so validation requests no longer hit the dashboard (405).

## [0.3.4] - 2026-07-17

### Added
- **Gated first deploy for runtime configuration** — Interactive `l8b ship` now stages the first deployment (compose + target-node `.env`) without starting containers, pauses with **Awaiting runtime configuration**, and only starts after confirmation. Redeploys are unchanged. Interrupted first deploys can resume via **Resume deployment**.
- **Explicit first-deploy project states** — New projects start as `pending`, become `unconfigured` after staging (image/compose + runtime `.env`), then move to `deploying` on confirmed start. Dashboard badges distinguish **Pending** vs **Awaiting configuration**.

### Changed
- **CLI `ship` cleanup** — Deduplicated interactive and noninteractive compose deploy paths (shared load/build-summary/prepare helpers), removed unreachable Compose port scanning, and clarified first-deploy pause/resume wording.

### Fixed
- **Waker ignores unconfigured projects** — Opening a staged project's URL no longer auto-starts containers; status stays `unconfigured` until the user confirms start from the CLI.
- **Waker also blocks pending projects** — Opening a `pending` project's URL does not auto-start; the project stays paused until artifacts are staged and start is confirmed.

## [0.3.3] - 2026-07-17

### Fixed
- **Compose one-shot services** — Dependencies with `service_completed_successfully` are tracked as completed (`is_oneshot`), ignored for project health/degraded status, and skipped on partial wake recovery (orchestrator and agent).

## [0.3.2] - 2026-06-25

### Fixed
- **`l8b deploy` auto-redeploys existing projects** — Falls back from `POST /deploy` (create) to `PUT /deploy` (redeploy) on 409 Conflict, so CI re-runs no longer fail when the project already exists.

## [0.3.1] - 2026-06-25

### Fixed
- **Landing docs site** — Sticky sidebar, mobile drawer, hash-routed cross-doc links, and full markdown rendering (tables, CRLF, lists, blockquotes); synced architecture deep-dives into sidebar and deploy workflow.
- **Release docs assets** — Upload `openapi.json`, `cli-reference.md`, and `llms-full.txt` as an artifact so they reach the release job (and add the missing `cli-reference.md` to the release file list).

## [0.3.0] - 2026-06-24

### Added
- **Reserved-port check for `allow_raw_ports`** — LiteBin service ports (80, 443, 2019, 5080, 5083, 8443) are silently skipped instead of crashing on "port already allocated".
- **Port constants in `litebin-common`** — Single source of truth for Caddy (80/443/2019), orchestrator (5080), and agent (8443/5083) ports. Orchestrator and agent config fallbacks now reference these constants instead of inline string literals.
- **Volume permission handling for non-root containers** — Automatically chowns relative bind mount directories before starting a container, based on the image's `USER` directive.
- **OpenAPI spec** — Auto-generated via utoipa, served at `/openapi.json` and `/llms.txt`.
- **Scalar API docs** — Interactive API reference at `/docs` (orchestrator and landing site).
- "Allow raw ports" and "Allow Docker access" toggles to Deploy New App form (compose mode).
- **Biome for linting + formatting** — Replaced ESLint with Biome (Rust-based, 10–25x faster). Single `biome.json` config, pre-commit hook via nano-staged.
- **Agent image inspect endpoint** — `GET /images/inspect` resolves any image reference to its sha256 digest. Used by orchestrator for digest-based cleanup on remote nodes.
- **Admin password reset subcommand** — `litebin-orchestrator reset-password` for recovering from a forgotten admin password (run via `docker exec`); not exposed via HTTP.

### Changed
- **Digest-based image cleanup on redeploy** — Old Docker images are now removed by their sha256 digest after redeploy, including same-tag updates (e.g. `nginx:latest` → `nginx:latest`). Previously, same-tag redeploys left the old version as a dangling image forever.
- **Per-service image cleanup on project deletion** — Deleting a multi-service project now cleans up all service images (not just the public service). Images shared with other projects are skipped automatically.
- Extracted `capture_service_digests()` helper to deduplicate digest capture logic across deploy, compose, and recreate flows.
- **Frontend code cleanup** — Extracted `HomePage` from `App.tsx` into its own component with custom hooks (`useHomeData`, `useSettings`, `useNodes`).
- **Typed status enums** — Replaced bare status strings with enums across backend and frontend.
- **Graceful shutdown** — Orchestrator and agent handle SIGTERM/SIGINT: stop accepting new connections, drain in-flight requests, cancel background tasks (janitor, heartbeat, activity tracker, route sync, periodic sync), close DB pool, and log each step.
- **Startup complete log** — Single clear `"startup complete — accepting connections"` log with addr, domain, and version after all init is done (replaces the ambiguous `"starting server"` message).
- **Typed Docker error classification** — `DockerErrorKind` enum replaces fragile `e.to_string().contains("404")` checks with pattern-matching on bollard error variants.
- **Split `docker.rs` into modules** — `litebin-common/src/docker/` now has `mod.rs` (types + struct), `container.rs` (lifecycle), `image.rs` (images, networking, volumes, stats), `tests.rs`.
- **Typed DB/Cloudflare error matching** — UNIQUE constraint detection uses SQLite error code 2067 instead of string matching. Cloudflare duplicate-record detection uses error code 81057 instead of message string.
- **Module splits** — Split large route files into directory modules: agent `containers.rs`, agent `waker.rs`, orchestrator `waker.rs`. Public APIs unchanged.
- **Shared code** — Extracted proxy utilities (`HOP_BY_HOP`, `is_hop_by_hop`, `wants_json`) to `litebin-common`, deduplicated `COMPOSE_FILE_NAMES` across agent/orchestrator/CLI, extracted `is_windows_drive_path()` helper.

### Fixed
- **All Biome lint errors** — Fixed 47 lint errors (a11y: button types, label associations, SVG titles, keyboard handlers; correctness: exhaustive deps, no-redeclare; suspicious: array index keys).
- **Pre-existing TypeScript errors in `ProjectCard`** — API functions `redeployProject`, `stopProject`, `startProject`, `deleteProject` now return `Promise<string[] | undefined>` to match the `handleAction` signature.
- **Silent failure logging** — DB errors and status transition failures that were silently discarded now produce `tracing::warn`/`error` logs. Existence guards (duplicate checks, TLS validation) return 500 on DB error instead of silently bypassing guards.
- **Delete modal volume classification** — Relative bind mounts (`./data`) were incorrectly shown as "Absolute bind mounts — not removed". Scoped paths now use `projects/{id}/...` instead of container-internal `/app/projects/...`.
- **Silent deserialization** — Volume and metadata JSON serialization failures no longer silently produce empty strings (which destroyed data on disk/DB). Volume deserialization failures now log errors instead of silently dropping volumes. `serialize_volumes()` helper returns `None` on failure.
- **DNS sync safety** — DB failure during DNS sync now aborts instead of proceeding with empty project list (which would delete all DNS records). Master Caddy `/load` failure now returns error instead of silently continuing.
- **Silent agent HTTP call logging** — Container stop/remove/cleanup calls to remote agents now log errors instead of silently discarding them. Waker no longer transitions to "Running" on non-JSON agent response.
- **Missing `docker-compose.yaml` in scan** — Docker scan now checks all compose file name variants.
- **Docker socket handling** — Containers with `/var/run/docker.sock` mounts no longer fail deploy/recreate when "Allow Docker access" is disabled; socket is stripped silently with a warning toast. Proxy sidecar waits for network readiness before dependent services start.
- **Non-fatal port mapping** — `run_service_container` no longer fails when a container exits immediately (e.g. missing docker.sock); returns port=0 and lets status polling resolve it.
- **PHP-style entrypoint init** — Added `FOWNER` + `FSETID` to the capability whitelist. Fixes PHP/Apache images that fail with `chmod: /tmp: Operation not permitted` during init.
- **Compose-only deploys stuck on "starting"** — Route resolver skipped projects with `mapped_port = NULL`, but compose deploys never bind host ports. Changed the guard to check `internal_port` (the in-container listen port), which is what the upstream dial address is actually built from.

## [0.2.17] - 2026-05-08

- **Async deploys with progress** — Deploy endpoints run in background, dashboard shows live progress modal with streaming logs, CLI polls with inline output. `l8b status --project <id>` shows deploy status and logs.
- **Local image check before pull** — Skips Docker Hub pull when image exists locally (deploy, start, recreate, waker). Only Redeploy force-pulls. Reduces rate limit usage.
- **Docker pull progress in deploy logs** — Deploy logs now show Docker's native pull output (layer status, download progress).
- **Docker Hub auth and tag fix** — `LITEBIN_REGISTRY_AUTH` / `LITEBIN_REGISTRY_URL` env vars for registry authentication. Fixed Docker API pulling all tags when no tag specified (now defaults to `:latest`).

## [0.2.16] - 2026-05-06

- Fix redeploy resetting sleep and per-service resource settings — Preserves `auto_stop_enabled`/`auto_start_enabled` on redeploy when not explicitly provided. Preserves dashboard-set memory/CPU overrides on compose redeploy when compose YAML doesn't specify them.
- Fix single-service compose showing "web" as service name — Now queries `project_services` for the real service name.

## [0.2.15] - 2026-05-06

### Added
- **Deploy Docker Compose from dashboard** — "Deploy New App" modal now has a toggle between "Docker Image" and "Docker Compose" modes. Compose mode shows a textarea to paste compose YAML with prebuilt images. Settings (sleep, resources, node picker) are shared across both modes.
- **`deploy_type` column** — Projects now track whether they were deployed as `"image"` or `"compose"`. Used in the dashboard to show compose-specific UI (Docker Compose label with service count, readonly image/port, hidden command override, services badge) for all compose projects including single-service ones.

### Fixed
- Fix HTTP→HTTPS redirect not working — Caddy JSON config with `listen: [":80", ":443"]` on a single server doesn't auto-redirect like the Caddyfile adapter does. Added an explicit 308 redirect route to all client-facing config generators (`sync_routes`, `MasterProxyRouter`, cloudflare master/agent). Skipped for localhost domains.
- Fix compose deploy routing 503 — Caddy was dialing `litebin-{id}:{port}` (hardcoded single-service name) for single-service compose projects, but the actual container is named `litebin-{id}.{service_name}`. Now always queries `project_services` for the real service name, so compose projects route correctly regardless of service count.
- Fix stop button not showing "Stopping" state — Stop button now uses the same `handleAction` pattern as start/redeploy, setting a loading state immediately on click instead of fire-and-forget. Shows spinner while the API responds.

### Changed
- Dashboard cleanups
- **Docker image pull progress logging** — Pull progress (layer, status, download %) now logged at info level instead of debug, making it visible in orchestrator logs without debug mode.

## [0.2.14] - 2026-05-01

### Added
- **`l8b status` command** — Shows CLI version, login state, server version, logged-in user, nodes (one per line with name, status, version, architecture), and total project count. Shows login hints when not authenticated. Powered by a single `GET /status` API call.

### Fixed
- Fix docker-socket-proxy not stopping with the project — all stop paths (manual, janitor, remote) now stop the proxy when `allow_docker_access` is enabled. Added compose labels so Docker Desktop groups it with the project.

## [0.2.13] - 2026-04-30

### Fixed
- Fix bind mount data not appearing on host filesystem — `scope_volume_source` returned a container-internal path (`/app/projects/...`) that Docker resolved on the host where that path doesn't exist. Now the orchestrator/agent auto-detect the host-side path by inspecting their own container mounts via Docker API, and translate bind mount paths before sending them to Docker.
- Fix global default memory/CPU settings not applying to new deploys — `DockerManager` was initialized with hardcoded 256MB/0.5 CPU constants and never read from the settings table. Now reads actual defaults from DB at startup and updates live when settings change (no restart needed).
- Fix CPU % always showing 0% on project cards — Docker stats API with `one_shot: true` returns a single instantaneous snapshot with no previous sample to compute a delta against. Now caches previous CPU samples per container and computes deltas between consecutive readings. First poll returns 0% (expected), subsequent polls show accurate values.
- Fix DNS records removed for stopped projects — DNS sync only kept records for running, degraded, or stopped projects with a custom domain. Stopped projects without a custom domain (e.g. subdomain-only projects) lost their DNS entry, breaking auto-wake. Simplified to a single query over all projects regardless of status — DNS is only removed when a project is deleted, never when stopped.

## [0.2.12] - 2026-04-29

### Fixed
- Fix relative bind mounts (e.g. `./data:/container/path`) in compose failing with "invalid characters for a local volume name". The path was not resolved to an absolute path, so Docker treated it as a named volume.

## [0.2.11] - 2026-04-29

### Added
- **Docker socket proxy** — Per-project "Allow Docker access" toggle injects a `docker-socket-proxy` service into multi-service projects, restricted to only the project's own containers via label filtering. Enables inter-service container management (exec, logs, stats, restart) without exposing the Docker socket. Follows the same pattern as "Allow raw ports" — toggle in dashboard, flag in DB, pushed to agent.
- **Standard Docker Compose labels** — All containers now get `com.docker.compose.service` and `com.docker.compose.project` labels, matching what Docker Compose sets natively. Enables tooling that relies on standard compose labels (e.g., service discovery by label).
- **Raw port support for compose deployments** — Per-project "Allow raw ports" toggle (dashboard → Settings → General) exposes all compose service ports directly on the host (TCP/UDP), bypassing Caddy. Enables game servers (UDP), databases, and voice servers alongside HTTP apps. Off by default — only affects multi-service compose projects and takes effect on restart.
- **compose-bollard: `stdin_open`, `tty`, `restart`** — Compose fields now pass through to Docker container config. Supports interactive shells (e.g. sending commands to Minecraft server) and custom restart policies (`always`, `unless-stopped`). LiteBin's default `restart: no` only applies when compose doesn't specify one.

### Fixed
- Fix raw port binding using ephemeral port (`0`) instead of the compose-declared port. Raw ports now bind to their actual port number (e.g., `19132/udp` binds host port 19132, not a random one).
- Fix www→bare domain redirect producing a trailing `}` in the URL. The Caddy placeholder `{uri}` was double-escaped in the Rust format string.

### Changed
- **Multi-service routing now goes directly to containers** — All running projects (single and multi-service) get direct Caddy→container routes. Previously, multi-service projects always proxied through the orchestrator, which broke WebSocket, gRPC, SSE, and other non-HTTP protocols. Caddy's 502/503/504 fallback to the orchestrator handles auto-wake when containers are down. Service health is now handled by Docker's `restart: unless-stopped` policy (recommended in compose files) instead of per-request orchestrator health checks.

## [0.2.10] - 2026-04-27

### Added
- **Cross-platform builds** — `l8b ship` and `l8b deploy` now automatically detect the target server's architecture and build Docker images for the correct platform (`linux/arm64`, `linux/amd64`, etc.). Fixes `exec format error` when deploying from an x86_64 machine to an ARM VPS (or vice versa). Works for Dockerfile, Railpack, and compose builds.
- **Node architecture and recommendation** — `/nodes` API now returns each node's `architecture` and a `recommended` flag (least-loaded online node). CLI node picker shows architecture and pre-selects the recommended node. "Auto" option removed — nodes are always explicitly selected.

### Fixed
- Fix per-service settings (memory/CPU) failing with "service not found" for multi-service projects — the handler queried a non-existent `id` column on `project_services`.

### Changed
- Node picker in `l8b ship` now shows architecture (e.g. `arm64`) and marks the recommended node instead of offering an "Auto (least loaded)" option.

## [0.2.9] - 2026-04-26

### Changed
- Unified single-service and multi-service code paths — `start_services()` / `stop_services()` now handle both, eliminating ~500 lines of duplicated branching. Waker uses one running path for all local projects.
- Stats endpoint performance — removed per-poll Docker sync (relies on 60s periodic sync), parallelized container stats queries, added disk cache for stopped containers.
- Custom routes auto-wake — added `handle_response` to catch 502/503/504 on custom path/subdomain routes and proxy to orchestrator for wake.

### Fixed
- Fix partial service start setting project to "running" when other services are still stopped — now correctly derives status from aggregate service states.
- Fix remote multi-service stop/start only operating on one container — now stops all service containers and uses batch-run for start.
- Fix waker marking all services "running" on agent wake report — now updates only the specific service and derives project status.
- Fix janitor failing to stop containers — pre-loads container IDs before status transition (which was clearing them).

## [0.2.8] - 2026-04-26

### Fixed
- Fix on-demand TLS certificate issuance failing — `waker_intercept` middleware was intercepting Caddy's internal permission check (`/caddy/ask`) because the request came from the orchestrator's Docker hostname, which wasn't in the middleware's allowlist. Internal container requests now pass through to route handlers.

### Changed
- Centralize project status management — replace 67+ raw SQL writes with a single `status` module for atomic cross-table updates, add a SQLite trigger safety net and 60s periodic Docker reconciliation.
- Align agent update flow with master — agent update now shows changelog link, version selection (latest or specific), restart confirmation prompt, and post-restart container health verification.
- Installing agent when agent is already running now redirects to the update flow (same as master already did).
- `install.sh` warns when installing agent on a server that already has master installed (or vice versa), since they typically run on separate servers.
- Filter `docker compose` output during master start/restart to show only container lifecycle events, hiding verbose build steps.

## [0.2.7] - 2026-04-26

### Fixed
- Fix image upload failing with "No such image" — `docker save` produces OCI format tars where the config digest (`docker inspect {{.Id}}`) differs from the manifest digest (assigned by `docker load`). CLI now sends the image tag instead of the config digest. The upload endpoint (orchestrator and agent) resolves the tag to the actual Docker-assigned image ID after loading and returns it, so the deploy step uses the correct reference.
- Fix `--node` flag overriding project's sticky node on redeploy — `deploy` command now checks if the project already has a `node_id` and ignores `--node` if they differ, keeping projects on their original node.

### Changed
- Node selection prompt no longer shows on redeploy when project has a `node_id` set — `ship` reuses the existing node automatically, only prompting for new projects or projects without a pinned node.

## [0.2.6] - 2026-04-26

### Fixed
- Fix image upload falsely failing — `docker save` produces OCI format tars where the image config digest differs from the manifest digest. The upload verification was inspecting by config digest (from `docker inspect {{.Id}}`) but Docker loads OCI images using the manifest digest. Removed the faulty verification since `import_image_stream` already reports errors on actual load failures.
- Fix single-service status sync — stop/start endpoints now update `project_services.status` in addition to `projects.status`, preventing stale "running" entries after stop and dashboard showing "degraded" incorrectly.

## [0.2.5] - 2026-04-26

### Fixed
- Fix image load silently failing on server — switched from `import_image_stream` (sends wrong `Content-Type: application/json`) to `import_image` (correct `Content-Type: application/x-tar`). Docker on some versions rejects the wrong content type silently, reporting success without actually loading the image.
- Fix single-service redeploy on remote agent pulling `sha256:` images from registry — `run_container` now skips registry pull for pre-loaded images (matching the existing fix in `batch_run` for compose).

## [0.2.4] - 2026-04-26

### Fixed
- Fix "degraded" status showing for running projects — service status wasn't synced back to "running" after a container recovered. Stats endpoint now updates `project_services.status` to "running" when a container is detected as running.
- Fix compose deploy — agent's `batch_run` was pulling pre-loaded `sha256:` images from a registry before creating containers. Now skips them during pull (matching orchestrator's local compose path).
- Added image verification after upload — `POST /images/upload` now confirms the image exists in Docker after loading, returning an immediate error if the load silently failed.

## [0.2.3] - 2026-04-25

### Added
- **Node selection for compose deploys** — `l8b ship` now shows an interactive node picker when multiple nodes are online (default: "Auto" lets the server pick least-loaded). `l8b deploy --compose --node <id>` for non-interactive CI usage. Images and deploy requests now always go to the same node, fixing a mismatch that caused "project not found" errors on remote agents.

### Fixed
- Compose deploy now persists `node_id` to the projects table, making redeploys sticky to the original node (matching single-service deploy behavior)

## [0.2.2] - 2026-04-25

### Changed
- Consolidate Caddy API path matchers into `ORCHESTRATOR_API_PATHS` constant in `litebin-common/src/caddy.rs` — single source of truth used by all three config builders (master_proxy, cloudflare_dns, CaddyRouter). Install script Caddyfile templates updated to match.

## [0.2.1] - 2026-04-25

### Fixed
- Fix `/deploy/compose` returning 404 — Caddyfile and dynamic Caddy config were missing the `/deploy/*` path matcher, so compose deploy requests fell through to the dashboard instead of the orchestrator

## [0.2.0] - 2026-04-25

### Added
- **Docker Compose support** — deploy multi-service apps directly from a `docker-compose.yml`. New `compose-bollard` crate parses and validates Compose YAML, converting it to bollard Docker API configs. Compose deploy runs each service as its own container on a per-project Docker network.
- **Multi-service architecture** — projects can now run multiple containers (e.g. frontend + API + DB). New `project_services` and `project_volumes` database tables, per-project Docker networks, and full lifecycle management (start/stop/delete/recreate individual services).
- **Volume persistence** — containers can persist data across recreations and redeployments. Supports named volumes and bind mounts scoped to `projects/{id}/data/`. Managed via API, CLI, and dashboard.
- **Custom route proxy** — define custom routing rules (path-based, subdomain-based) via dashboard or CLI. Routes are applied through Caddy on the agent.
- **Waker returns 503 during wake** — waking containers now return `503 Service Unavailable` with JSON for API clients instead of a 200 spinner page, preventing SEO bots from indexing the loading state.
- **Compose variable interpolation** — `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, `$VAR`, and `$$` syntax in compose files. Variables resolved from compose `environment` section, `.env` files, and system env.
- **CLI: `--compose` and `--service` flags** — `deploy` command auto-detects compose files and deploys as multi-service. Use `--service api --service worker` to selectively build specific services (CI-friendly, no interactive prompts).
- **CLI: object-form `build:` support** — `build: { context: ./api, dockerfile: Dockerfile.dev }` in compose files now correctly uses the specified context and Dockerfile.
- **Dashboard: service management** — view and manage individual services within a project. New modular ProjectCard components (service settings, sleep controls, redeploy, service selection).
- **Dashboard: log viewer** — improved log viewer component with better streaming and display.
- **Dashboard: volume management** — view and manage project volumes from the dashboard.
- **New documentation** — [multi-service.md](docs/multi-service.md), [volumes.md](docs/volumes.md), [waker.md](docs/waker.md), [user-flows.md](docs/user-flows.md)

### Fixed
- Fix multi-service remote routing using node UUID as hostname instead of the actual node host address — remote multi-service and degraded projects now correctly resolve the node's `host` from the database.

### Changed
- Refactored dashboard ProjectCard into modular sub-components for better maintainability
- Improved agent container management for multi-service projects (waker, janitor support)
- Streamlined volume handling across orchestrator and agent
- Improved routing helpers for custom routes and multi-service networks

## [0.1.25] - 2026-04-13

### Fixed
- Fix agent Caddy self-referencing loop in cloudflare_dns mode — orchestrator pushed `{agent_ip}:443` as the upstream in the agent's Caddy config, causing it to proxy to itself instead of the app container (`litebin-{id}:{port}`). Added `container_upstream` field to `ProjectRoute` so the agent Caddy uses the correct Docker network address.

### Added
- Troubleshooting FAQ ([docs/faq.md](docs/faq.md)) — common issues with mTLS certs, agent connectivity, Docker logs, and firewalls

### Changed
- Added "Further Reading" links to architecture docs and centralized doc navigation from README

## [0.1.24] - 2026-04-13

### Fixed
- Fix Cloudflare DNS records never being created — response structs (`CfListResult`, `CfSingleResult`) had incorrect nesting that didn't match the Cloudflare API format, causing all API responses to fail parsing
- Fix Cloudflare credentials not used on first settings save — credentials were saved to DB after the router hot-swap, so the hot-swap always read empty values
- Fix local node `public_ip` always `None` in route resolution — now reads from the `nodes` table instead of hardcoding `None`
- Fix agent `public_ip` being overwritten by auto-detection — dashboard-set value now takes priority, agent-reported IP only fills in when empty
- Fix deploy modal not scrollable when content overflows viewport

### Changed
- Improve Cloudflare API error messages — include HTTP status code and response body in all error logs for easier debugging
- Skip Cloudflare DNS API calls during periodic janitor checks — DNS sync only runs on actual changes (deploy, stop, delete, settings)
- Add DNS setup instructions to install script output (mode-specific: wildcard for master_proxy, two records for cloudflare_dns)
- Auto-detect server public IP during install for DNS instructions
- Change settings tab buttons (Routing Mode, Token Scope) to cyan to distinguish from action buttons
- Update documentation in README, multi-server docs, and janitor docs

## [0.1.23] - 2026-04-12

### Fixed
- Fix agent Caddy rejecting config with "on-demand TLS cannot be enabled without a permission module" — add `on_demand.permission` endpoint (`/internal/caddy-ask`) that allows subdomains of the configured domain and custom domain routes from the orchestrator
- Fix agent data not persisting across container recreations — Dockerfile was missing `WORKDIR /etc/litebin`, so registration/caddy config were written to `/data/` instead of the persistent volume at `/etc/litebin/data/`

## [0.1.22] - 2026-04-12

###Fixed
- Fix agent never routing to newly deployed containers — `run_container` and `recreate_container` now call `rebuild_local_caddy` after starting (was only done in `start_container`)

## [0.1.21] - 2026-04-12

### Fixed
- Fix agent Caddy TLS cert loading — use inline PEM (`load_pem`) instead of file paths (`load_files`), eliminating the need for certs to exist inside the Caddy container's filesystem
- Remove certs volume mount from agent Caddy container (no longer needed — certs are embedded inline in the Caddy JSON config)
- Fix `regenerate_certs` failing silently — agent cert copy was attempted before cert generation, causing abort under `set -euo pipefail`
- Fix agent Caddy failing to start on fresh install — missing `ensure_agent_network` call before creating the Caddy container
- Fix `install_agent` not writing `.version` file — first update always showed "unknown" version
- Fix title glitch stacking animation

## [0.1.20] - 2026-04-12

### Fixed
- Fix agent Caddy admin API unreachable from agent container — bind to `0.0.0.0:2019` via Caddyfile (was `localhost:2019` by default, only reachable inside the Caddy container itself)
- Preserve original Host header when proxying to remote agents over TLS — Caddy 2.11+ auto-rewrites Host to upstream address, breaking agent route matching

## [0.1.19] - 2026-04-12

### Changed
- Fix Caddy TLS transport config — use `root_ca_pem_files` instead of `trust_pool` (reverse proxy transport doesn't support `trust_pool`)
- Extract `run_agent_caddy()` function to deduplicate agent Caddy container creation in install and update flows
- Agent update now always recreates the Caddy sidecar (picks up image and config changes)

## [0.1.18] - 2026-04-12

### Changed
- Pin Caddy image to `caddy:2.11.2-alpine` across all install scripts and compose files (master + agent)

## [0.1.17] - 2026-04-12

### Added
- TLS-encrypted traffic between master and remote agent Caddys using existing mTLS PKI (agent.pem + ca.pem)
- Agent Caddy loads TLS cert on startup (base config with catch-all 502)
- Caddy config rebuild after API container start (`POST /containers/start`)
- Internal wake server on agent (port 8444, plain HTTP, Docker network only) for `cloudflare_dns` mode wake flow

### Fixed
- Fix agent Caddy upstream resolution — use Docker network container names (`litebin-{id}:{port}`) instead of `localhost:{mapped_port}` (broken when Caddy runs as a separate container)
- Fix `master_proxy` mode for remote agents — traffic now routes through agent Caddy sidecar over TLS instead of direct container port access (which was unreachable and unencrypted)
- Fix agent Caddy catch-all routing to static 502 instead of proxying to mTLS API port
- Fix `cloudflare_dns` mode agent Caddy catch-all — now uses internal wake server at `litebin-agent:8444`

## [0.1.16] - 2026-04-12

### Added
- Agent update flow (`bash -s update`) — auto-detects master vs agent, shows version diff, downloads and restarts
- Cert bundle retrieval — `bash -s certs` interactive menu to generate, regenerate, or show existing bundle without re-running master setup
- Docker network and Caddy sidecar creation in agent install, update, and cert update flows
- Agent port detection from previous container on update/cert-update (preserves custom ports)

### Fixed
- Fix `show_cert_bundle` failing when agent cert files not saved to master certs directory
- Fix master detection in update using `-d` (directory test) instead of `-f` (file test) for `docker-compose.yml`
- Fix agent cert update port detection running after container removal (always fell back to 5083)

## [0.1.15] - 2026-04-11

### Fixed
- Fix agent mTLS connection — add `subjectAltName` to agent certificate (rustls/webpki requires SAN, no CN fallback)

## [0.1.14] - 2026-04-11

### Added
- Show disk usage for stopped/sleeping projects with in-memory cache — agent only queried on cache miss (e.g. after restart)

### Fixed
- Fix orchestrator crash on startup — explicitly install rustls `ring` crypto provider before TLS init

## [0.1.13] - 2026-04-11

### Fixed
- Fix mTLS connection to agents — skip hostname verification when connecting by IP (agent certs have no IP SAN)
- Fix Add Agent wizard creating duplicate entries on connect retry, show result screen with retry button
- Add Connect button on pending_setup agent cards for retrying without re-adding

## [0.1.12] - 2026-04-11

### Fixed
- Fix cert path mismatch in `configure_multi_server`, default agent port to 5083 in dashboard, add delete confirmation modal for agents
- Use `curl -fsSL` (fail early on errors) instead of `curl -sSL` across all install scripts

## [0.1.11] - 2026-04-11

### Fixed
- Agent TLS key loading — use SEC1 parser matching `openssl ecparam` key format

## [0.1.10] - 2026-04-11

### Fixed
- Agent crash on startup — explicitly install rustls `ring` crypto provider before TLS init

## [0.1.9] - 2026-04-11

### Changed
- Landing page migrated to Vite + Tailwind CSS v4 (replaces CDN, adds HMR dev server)
- Reduced mTLS cert bundle size ~6x by replacing tar with PEM concatenation + gzip compression
- Renamed "Node" to "Agent" across install script and dashboard navigation
- Removed unnecessary prompts from agent and cert setup (master URL, node name)
- Fixed agent crash on startup due to missing rustls crypto provider (added `ring` feature)

## [0.1.8] - 2026-04-11

### Changed
- Landing page migrated to Vite + Tailwind CSS v4 (replaces CDN, adds HMR dev server)
- Reduced mTLS cert bundle size ~6x by replacing tar with PEM concatenation + gzip compression
- Renamed "Node" to "Agent" across install script and dashboard navigation
- Removed unnecessary prompts from agent and cert setup (master URL, node name)
- Fixed agent crash on startup due to missing rustls crypto provider (added `ring` feature)

## [0.1.7] - 2026-04-11

### Added
- `FLUSH_INTERVAL_SECS` env var — configurable activity tracker flush interval (default 60s), controls how often idle timestamps are written to the database

## [0.1.6] - 2026-04-10

### Added
- Traffic-based idle detection — janitor now stops only truly idle projects by tracking real HTTP requests via Caddy access logs instead of relying solely on deploy/wake timestamps
- Unified custom domain wake for both master proxy and cloudflare_dns modes via Caddy Host header rewrite
- Agent now checks `auto_start_enabled` before waking — shows "currently offline" page when disabled
- Agent Caddy config persistence (`data/caddy-config.json`) — survives restarts, used as base for local rebuilds
- Agent project metadata persistence (`data/project-meta.json`) — stores `auto_start_enabled` flags pushed by orchestrator
- `POST /internal/project-meta` endpoint on agent for receiving project metadata from orchestrator
- Sleeping custom domain routes upgraded to direct proxy on agent local wake

### Changed
- Agent waker rebuilds Caddy from persisted orchestrator config instead of building from scratch

## [0.1.4] - 2026-04-10

### Added
- `workflow_dispatch` trigger support in GitHub Actions deploy example
- CLI PATH setup instructions in quickstart guide (Linux/macOS and Windows)
- Mobile navigation menu on landing page

### Fixed
- `bash -s` typo in install commands across README and docs

## [0.1.3]

### Added
- Multi-node mTLS agent setup via `bash -s certs`
- Agent cert bundle update flow (`bash -s agent --update-certs`)
- Cloudflare DNS routing mode

### Changed
- Apps now sleep to zero after configurable idle timeout (`DEFAULT_AUTO_STOP_MINS`)

## [0.1.0]

### Added
- Initial release
- Master server setup (orchestrator + dashboard + Caddy)
- Agent (worker node) support with mTLS
- CLI (`l8b`) — `ship`, `deploy`, `login` commands
- GitHub Actions deploy action (`mtsandeep/l8bin-action`)
- Scale-to-zero: apps sleep on idle, wake on first request
- Dashboard: deploy, manage, and monitor apps
- Automatic TLS via Caddy
