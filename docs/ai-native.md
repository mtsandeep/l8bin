# AI-Native LiteBin

LiteBin is fully operable by a coding agent: deploy → verify → troubleshoot → set env → restart runs end-to-end with no dashboard visits and no SSH. The recurring loop is 100% agent; one-time trust anchors stay human.

| Done by agent (every deploy) | Done by human (once per server lifetime) |
|---|---|
| Build, deploy, verify healthy | Install server (installer), DNS records |
| Read logs, diagnose failures | Create admin account |
| Set/update runtime env | Approve agent pairing (device flow) |
| Restart/stop/start | Hold secrets (env files / sops keys) |
| Domains (`l8b domain set`) | Custom-domain DNS (master_proxy mode — LiteBin can't touch arbitrary DNS providers) |

Entry points: [agents.md](agents.md) (drop-in AGENTS.md template + MCP wiring), [cli-reference.md](cli-reference.md) (verbs + flags), [api-reference.md](api-reference.md) (HTTP surface + scopes).

## Token scopes

`deploy_tokens.scope` is a fixed cumulative ladder: `read ⊂ deploy ⊂ manage ⊂ admin` (ordered in [`litebin-common/src/types.rs`](../litebin-common/src/types.rs)). Tokens bind to one project (`project_id`, null = global); bound tokens only reach their project's paths. `GET /whoami` reports the active credential: token name, scope, binding (or the session user).

Route groups in [`orchestrator/src/app.rs`](../orchestrator/src/app.rs) — session-or-token, 401 JSON:

| Scope | Allows | Routes |
|---|---|---|
| `read` | see state | GET `/projects`, `/projects/{id}`, `/stats`, `/logs`, `/deploy-logs`, `/disk-usage`, `/env` (masked only), `/nodes`, `/meta`, `/whoami` |
| `deploy` | + ship code | `/deploy` POST/PUT, `/deploy/compose`, `/compose/validate`, `/images/*` |
| `manage` | + operate apps | PUT `/env`, stop/start/recreate, service start/stop/restart, project + service settings, routes, capabilities |
| `admin` | + destructive/platform | project delete, volume deletes, node lifecycle, global settings + DNS/domain ops, system stats, scan |

Rules:

- Token CRUD is session-only — tokens must not mint tokens. `admin` tokens are minted only via a dashboard session (pairing approval shows an explicit warning).
- `GET /settings` is admin-only and carries Cloudflare secrets; `GET /meta` (read) is the safe URL-computation endpoint (`domain`, `dashboard_subdomain`, `poke_subdomain`, `routing_mode`, `version`).
- `POST /projects` binds a session user; deploy endpoints stage projects instead, so token deploys never need it.
- Sessions pass everything (dashboard back-compat). Session-only groups redirect to login (307); agents only touch the scope-guarded groups, which are 401-JSON clean.

## Machine output contract

Every CLI command takes `--json` (or `L8B_JSON=1`). Success: one `{"ok": true, ...}` object on stdout, progress chatter on stderr, secrets never serialized. Failure: `{"ok": false, "error": {"message", "hint"}}` with exit 1 — `hint` carries the exact next command/flag.

- `l8b status --wait` exits 0 only when the project reaches running; `--healthy` additionally probes the live URL (wake-tolerant) and reports `http_status` + `latency_ms`, exiting 0 only on 2xx. This is the definitive "is it up?" check.
- Deploy timeouts exit 0 with `"status": "deploying (timed out waiting)"` — exit codes are reserved for transport/command failures; agents read the field.
- Result structs ([`cli/src/commands/`](../cli/src/commands/)) are typed `Serialize` and are exactly what the MCP tools return — one shape, two renderings.

## Runtime env: write-only

No plaintext read-back endpoint exists, by design — a read-back endpoint would turn every leaked token into a full secret dump. Recovery is SSH to your own file.

| Rule | Where |
|---|---|
| GET `/projects/{id}/env` returns masked previews only (`••••••••` — no length or first-char leak) | [`orchestrator/src/routes/env.rs`](../orchestrator/src/routes/env.rs) |
| PUT (merge/replace) requires `manage`; 0600 service-owned files; 64 KB cap; audit logs keys, never values | same |
| Single-node writes the local `litebin/projects/<id>/.env`; remote nodes go through the agent's `/internal/env` ([`agent/src/routes/env.rs`](../agent/src/routes/env.rs)) | [`AgentClient::write_env_file`](../orchestrator/src/nodes/client.rs) |
| Changes apply on next container start/recreate; `pending_apply` is surfaced until then (dashboard modal, CLI output) | same |
| CLI input is file/stdin only (`l8b env push --file \| --stdin [--replace] [--apply]`) — `KEY=VAL` args leak via shell history and transcripts | [`cli/src/commands/env.rs`](../cli/src/commands/env.rs) |

Recommended agent workflow: commit encrypted env to the repo and run `sops -d env.production | l8b env push <project> --stdin` — plaintext never touches git or the agent's context.

Threat model: transit, read-back, scope abuse, filesystem perms, and audit are defensible; root on the node is not (reads `/proc/<pid>/environ` regardless of storage design). Encrypted-at-rest with a local key is optional future hardening, not current behavior.

## Pairing (`l8b login`)

GitHub-CLI-style device flow ([`orchestrator/src/routes/device_auth.rs`](../orchestrator/src/routes/device_auth.rs), [`cli/src/auth.rs`](../cli/src/auth.rs)):

1. CLI posts `/auth/device/start` → prints `{server}/connect?code=L8B-XXXXXX` (122-bit device code, 600 s TTL).
2. User opens the URL in a browser that already has the dashboard session; the connect page shows scope radio + project binding and "only approve codes you initiated".
3. CLI polls `/auth/device/token` until approved, stores the token.

Logins are stored per server and coexist — no logout dance to switch instances; `server` in `l8b.toml` binds the workspace. CI keeps `L8B_SERVER` + `L8B_TOKEN` env vars. `l8b setup --server <url>` bootstraps the first admin then pairs (refuses in CI mode — trust boundary stays human).

Brute-force/spam defenses ([`orchestrator/src/rate_limit.rs`](../orchestrator/src/rate_limit.rs)): per-IP fixed-window limits — login 10/min, device/start 5/min, device/token 30/min — plus a 100-cap on pending codes; keyed on the last XFF hop (spoof-proof). Guessing is not the threat (122-bit codes, 256-bit tokens); spam is.

## `l8b.toml` + `init`

Three keys — `project`, `node`, `server` — nothing else. Port is sticky server-side; there is deliberately no env-file mapping (`deploy --env-file` covers it). Deploys auto-write it ([`cli/src/project_config.rs`](../cli/src/project_config.rs)); commit it, it holds no secrets. Any agent opening the repo discovers "this deploys via LiteBin" with zero arguments.

`l8b init --project <id>` writes it; `--mcp` also emits the workspace `.mcp.json`; `--agents` merges a managed `## Deploying (LiteBin)` section into `AGENTS.md` (idempotent, user content untouched — [`cli/src/agents_md.rs`](../cli/src/agents_md.rs)).

## MCP server

`l8b mcp` ([`cli/src/mcp.rs`](../cli/src/mcp.rs)): stdio JSON-RPC 2.0, spawned as a child process per agent session — no port, no daemon. It reads the same stored credential as the CLI; the agent config contains no secrets:

```json
{ "mcpServers": { "litebin": { "command": "npx", "args": ["-y", "l8bin-mcp"] } } }
```

15 tools (`setup`, `deploy`, `status`, `list`, `logs`, `env_list`, `env_push`, `stop`, `start`, `restart`, `url`, `domain_set`, `domain_remove`, `delete`, `doctor`) wrap the CLI verbs and return the same JSON structs. `setup` is a state machine — call it with no arguments and follow `state` + `next`; it pairs auth and binds the workspace (backed by the hidden `l8b __setup`). The npm shim (`l8bin-mcp`) downloads the `l8b` binary on first use (`L8B_BIN` overrides). Registry listings: [mcp-registries.md](mcp-registries.md).

## Env surfaces

Every surface can set env at deploy time or after; all write the same file.

| Surface | Deploy-time env | Post-deploy env |
|---|---|---|
| `l8b deploy` | `--env-file` — pushed (merge) after the deploy, then recreate applies it | `l8b env push --apply` |
| `l8b ship` | "Awaiting runtime configuration" pause: push a local `.env` / start now / pause | same pause on resume |
| Dashboard deploy dialog | env textarea (KEY=VALUE), pushed after staging | — |
| Dashboard project view | — | env modal: masked keys, `pending_apply`, write-only editor; unconfigured projects get "Save & start" |
| Direct file edit | `litebin/projects/<id>/.env` — unchanged | unchanged |

## Deliberate divergences from the original plan

- No `l8b env set` verb — merge-mode `push` (+ `--replace`) covers it.
- Masking is constant `••••••••`, not `k•••` + length (no key-length leak).
- GET env sits at `read` scope (masked-only listing makes it safe); the plan's table implied `manage`.
- Token CRUD stayed session-only instead of moving to `admin` scope.
- `deploy --env-file` pushes after the deploy completes and recreates, rather than between stage and first start.

## Not built (future polish)

OS keyring for stored credentials · `l8b logs -f` streaming · `volumes`/`routes`/`nodes`/`tokens` CLI verbs + the `l8b api` wrapper · `vault://`/`doppler://` secret references resolved at deploy · orchestrator-hosted HTTP `/mcp` endpoint · `doctor` DNS preflight.
