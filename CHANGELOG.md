# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed
- Reduced mTLS cert bundle size ~6x by replacing tar with PEM concatenation + gzip compression

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
