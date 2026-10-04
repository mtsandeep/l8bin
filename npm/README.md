# l8bin-mcp

[LiteBin](https://l8bin.com) MCP server — deploy and operate your self-hosted projects from any MCP client.

Your agent gets deploy, logs, health checks, and runtime secrets as first-class tools over [MCP](https://modelcontextprotocol.io). No dashboard visits, no memorized commands, no pasted passwords.

```json
{
  "mcpServers": {
    "litebin": {
      "command": "npx",
      "args": ["-y", "l8bin-mcp"]
    }
  }
}
```

Claude Code one-liner:

```sh
claude mcp add litebin -- npx -y l8bin-mcp
```

## How it works

This package is a thin shim: it runs `l8b mcp` (the MCP server ships inside the
`l8b` CLI) and downloads the platform binary from
[GitHub releases](https://github.com/mtsandeep/l8bin/releases) on first use. If
`l8b` is already on your PATH, the shim uses your install as-is. Set `L8B_BIN`
to point at a specific binary.

## Getting started

1. **A server running LiteBin** — one command on any VPS with Docker:
   `curl -fsSL https://l8b.in | bash` ([quickstart](https://l8bin.com/quickstart))
2. **Authenticate** — tell your agent *"set up litebin mcp"*, or run it yourself:

   ```sh
   npx -y l8bin-mcp   # first run fetches l8b (also verifies the download)
   l8b login --server https://your-l8bin.example.com --pair
   ```

   A short code prints; approve it at `/connect` in your dashboard and pick the
   token scope. Agents can run the pairing themselves and relay the code.

## Tools

14 tools over one stdio server: `deploy`, `status` (wait/healthy), `list`,
`logs`, `env_list`, `env_push`, `url`, `stop`, `start`, `restart`,
`domain_set`, `domain_remove`, `delete` (confirm-gated), `doctor`.

Tokens are scoped (`read → deploy → manage → admin`, optionally locked to one
project), secrets are write-only, and everything is revocable and audited.

- Docs: <https://l8bin.com/docs/>
- Agent guide: <https://l8bin.com/agents>
- llms.txt: <https://l8bin.com/llms.txt>
- GitHub: <https://github.com/mtsandeep/l8bin>

MIT © Sandeep MT
