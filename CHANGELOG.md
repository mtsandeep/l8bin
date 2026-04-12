# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
