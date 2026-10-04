# Publishing l8bin-mcp

npm publishing is automated: `publish-npm.yml` runs when the Release workflow
release, verifies the version matches the release tag, and publishes with
provenance via npm Trusted Publishing (OIDC — no token secret to manage).
One-time setup:

1. **Configure the trusted publisher** — on npmjs.com, package settings →
   Trusted Publishing → repository `mtsandeep/l8bin`, workflow
   `publish-npm.yml`. Trusted publishing is configured per existing package,
   so the very first publish (`0.1.0`) is done once manually: `npm login`,
   then `npm publish` from `npm/`.
2. **A GitHub release that includes the `mcp` subcommand** — the shim downloads
   the latest release binary; releases before `l8b mcp` existed cannot serve it.

## Canonical listing metadata

Reuse these across every registry form:

- **Name**: `l8bin` (pronounced LiteBin; package: `l8bin-mcp`)
- **Tagline**: Deploy and operate self-hosted projects from any MCP client
- **Short description**: LiteBin is a self-hosted PaaS for side projects. This
  MCP server gives coding agents deploy, logs, health checks and runtime
  secrets as first-class tools over stdio.
- **Long description**: 14 tools (deploy, status with wait/healthy, list, logs,
  env_list, env_push, url, stop, start, restart, domain_set, domain_remove,
  confirm-gated delete, doctor). Scoped tokens (read → deploy → manage →
  admin, optionally locked to a single project), write-only secrets, revocable
  and audited. The server wraps the `l8b` CLI and auto-downloads the platform
  binary on first use; auth is one-time device pairing approved from the
  dashboard.
- **Tags**: deployment, devops, docker, self-hosted, paas, hosting
- **Install config**:

  ```json
  { "mcpServers": { "litebin": { "command": "npx", "args": ["-y", "l8bin-mcp"] } } }
  ```

- **Links**: repo `https://github.com/mtsandeep/l8bin` · homepage
  `https://l8bin.com` · agent guide `https://l8bin.com/agents` · docs
  `https://l8bin.com/docs` · logo `https://l8bin.com/logo-square.png`

## Registries

| Registry | How | Notes |
| --- | --- | --- |
| Smithery | `https://smithery.ai/new`, point it at the repo | Reads `smithery.yaml` from the repo root; the web flow may regenerate it — keep the `npx -y l8bin-mcp` start command. |
| Glama | `https://glama.ai/mcp/servers/submit` | Form submission with the metadata above. |
| PulseMCP | `https://pulsemcp.com/submit` | Also propagates from canonical listings over time — submit manually anyway. |
| mcp.so | `https://mcp.so/submit` | Simple form; same metadata. |

The official `modelcontextprotocol/servers` repo is for reference servers —
not applicable.

## After listing

Registries and clients will point at `npx l8bin-mcp`. The publish workflow
syncs the package version to each release tag, so the npm page stays current
automatically — no manual version bumps. The shim always fetches the latest
GitHub release regardless of package version, so even a missed publish only
delays the npm listing, never the tool.
