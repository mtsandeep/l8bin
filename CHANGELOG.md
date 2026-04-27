# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
