# Publishing l8bin-mcp

npm publishing is automated: `publish-npm.yml` runs when the Release workflow
completes, verifies the version matches the release tag, and publishes with
provenance via npm Trusted Publishing (OIDC — no token secret to manage).
One-time setup (done):

1. Trusted publisher configured on npmjs.com (repository `mtsandeep/l8bin`,
   workflow `publish-npm.yml`); the `0.1.0` bootstrap publish is out.
2. Releases include the `mcp` subcommand — the shim downloads the latest
   release binary.

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

| Priority | Registry | How | Status / Why |
| --- | --- | --- | --- |
| 1 | awesome-mcp-servers (punkpeye) | GitHub PR adding l8bin under the relevant category | The curated list the official MCP repo points to; feeds developer trust, SEO, and what models know about us. |
| 2 | PulseMCP | `https://pulsemcp.com/submit` | Largest fully-open index, daily-updated; may auto-crawl the repo, but an explicit submit controls our metadata. |
| 3 | Glama | `https://glama.ai/mcp/servers/submit` | Largest raw count; visibility is filter-gated but presence costs nothing. |
| 4 | mcpservers.org | Directory submit form | Smaller community directory; quick to do. |
| 5 | ToolHive (Stacklok) | Investigate their vetting submission | Security-vetted registry — a good trust signal for a self-hosted tool, and early in a curated queue beats a late entry in an open one. |
| 6 | mcp.so | Directory submit form; submission goes through a review queue | **Submitted** — queued for review. Watch the live listing: scrapers picked a docker-compose command as the config initially; the submitted config is the `npx -y l8bin-mcp` JSON above. |

Skipped, deliberately:

- **Smithery** — requires a remote HTTP MCP server; we are stdio-only
  (`l8b mcp` / npx). If an orchestrator HTTP MCP endpoint ever ships, revisit.
- **BenchGecko** — too small to matter yet.
- **modelcontextprotocol/servers** — reference servers only, not applicable.

## After listing

Registries and clients will point at `npx l8bin-mcp`. The publish workflow
syncs the package version to each release tag, so the npm page stays current
automatically — no manual version bumps. The shim always fetches the latest
GitHub release regardless of package version, so even a missed publish only
delays the npm listing, never the tool.
