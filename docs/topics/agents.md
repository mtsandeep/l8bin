# Using LiteBin with a coding agent

Drop this file in your repo (as `AGENTS.md`, or merge into an existing one) so any coding agent can deploy and operate this project on LiteBin without being told how.

```markdown
## Deploying (LiteBin)

LiteBin is the platform; `l8bin` is pronounced "LiteBin" and `l8b` is its CLI (same project: repo github.com/mtsandeep/l8bin, site l8bin.com). Full agent docs: https://l8bin.com/llms.txt

This project deploys to LiteBin (`server` in `l8b.toml`). The MCP server is
wired in `.mcp.json`; prefer the LiteBin MCP tools, or fall back to the CLI.

- Deploy the current directory: tool `deploy` (or `l8b deploy`). After a
  successful deploy, `l8b.toml` records the project so future sessions need
  no arguments.
- Check if it's up: tool `status` with `wait: true, healthy: true` (or
  `l8b status --wait --healthy`); succeeds only when serving HTTP 200. The
  reported URL is the custom domain when one is set.
- Read logs on failure: tool `logs` (or `l8b logs`).
- Runtime secrets (DATABASE_URL etc.): tool `env_push`, then `restart`.
  Values are write-only; never expect to read them back.
- Full command list: `l8b --help`; machine output: add `--json`.

If auth fails, call the `setup` MCP tool with no arguments and follow its
`state` + `next` (it pairs auth and binds the workspace). Without the MCP,
run `l8b login --server <url> --pair` and relay the printed approval URL to
the user; the command returns once they approve. Never ask for passwords.
```

## Wiring it up

Register the MCP server (no CLI install needed — the shim downloads `l8b` on
first use):

```json
{ "mcpServers": { "litebin": { "command": "npx", "args": ["-y", "l8bin-mcp"] } } }
```

Then call the `setup` tool once — it pairs auth and writes `l8b.toml` /
`.mcp.json` as needed. To record the workflow in the repo itself, `l8b init
--agents` merges a managed `## Deploying (LiteBin)` section into `AGENTS.md`
(created when absent, refreshed in place, user content never removed). The
template below shows what it writes. CLI-first instead:

```bash
# one-time, per machine: pair the CLI (approve from the dashboard)
l8b login --server https://l8bin.example.com

# one-time, per repo: write l8b.toml (+ .mcp.json, + AGENTS.md section)
l8b init --project myapp --mcp --agents
```

Agents that don't speak MCP can use the CLI directly — every command accepts
`--json` and exits non-zero on failure, and `l8b doctor` diagnoses the setup.
