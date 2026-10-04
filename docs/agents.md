# Using LiteBin with a coding agent

Drop this file in your repo (as `AGENTS.md`, or merge into an existing one) so any coding agent can deploy and operate this project on LiteBin without being told how.

```markdown
## Deploying (LiteBin)

This project deploys to LiteBin. The `l8b` CLI is pre-configured via `l8b.toml`
and the MCP server is wired in `.mcp.json` — prefer the LiteBin MCP tools, or
fall back to the CLI.

- Deploy the current directory: tool `deploy` (or `l8b deploy`)
- Check if it's up: tool `status` with `wait: true, healthy: true` (or
  `l8b status --wait --healthy`) — succeeds only when serving HTTP 200
- Read logs on failure: tool `logs` (or `l8b logs`)
- Runtime secrets (DATABASE_URL etc.): tool `env_push`, then `restart`.
  Values are write-only — never expect to read them back.
- Full command list: `l8b --help`; machine output: add `--json`

If auth fails, ask the user for the LiteBin server URL, then run the pairing
yourself: `l8b login --server <url> --pair`. It prints an approval URL (the
code is embedded, the page pre-fills it) — relay it to the user and wait; the
command returns once they approve (they also pick the token's scope). Never
ask the user for their password.
```

## Wiring it up

```bash
# one-time, per machine: pair the CLI (approve from the dashboard)
l8b login --server https://l8bin.example.com

# one-time, per repo: write l8b.toml (+ .mcp.json for MCP clients)
l8b init --project myapp --port 3000 --mcp
```

Clients without a local `l8b` binary can use the npm shim instead, which
downloads it on first use:

```json
{ "mcpServers": { "litebin": { "command": "npx", "args": ["-y", "l8bin-mcp"] } } }
```

Agents that don't speak MCP can use the CLI directly — every command accepts
`--json` and exits non-zero on failure, and `l8b doctor` diagnoses the setup.
