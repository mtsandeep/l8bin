# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
