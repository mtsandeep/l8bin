# AI-Native LiteBin

Make LiteBin fully operable by a coding agent: a user says *"deploy this with litebin"* or *"is it up?"* and the agent handles it end-to-end — no dashboard visits, no memorized CLI commands.

---

## Honest Scope: Agent vs Human

The recurring loop is 100% agent. One-time trust anchors stay human — same trust shape users already accept from Vercel/Railway (signup, DNS, billing), minus the recurring dashboard visits.

| Done by agent (every deploy) | Done by human (once per server lifetime) |
|---|---|
| Build, deploy, verify healthy | Install server (installer), DNS records |
| Read logs, diagnose failures | Create admin account |
| Set/update runtime env (`l8b env`) | Approve agent pairing (device flow) |
| Restart/stop/start, scale knobs | Hold secrets (env files / sops keys) |
| Domains (cloudflare_dns mode) | Custom-domain DNS records (master_proxy mode — LiteBin can't touch arbitrary DNS providers) |

**The pitch:** install the skill/MCP once; everything that recurs is agent-handled; the human is the one-time trust anchor.

---

## Current State & Gaps (evidence)

- **CLI vocabulary is deploy-only.** Full surface: `deploy`, `ship` (interactive-only), `login`, `logout`, `status`, `cleanup`, `config` (`cli/src/main.rs`). No `logs`, `list`, `stop/start/restart`, `env`, `url`, `delete` — all of which exist as API endpoints but not commands.
- **Deploy tokens can't read anything back.** Tokens are accepted only by `/deploy*` and `/images/*` (`orchestrator/src/app.rs` deploy_routes group). Stats, logs, stop/start — session-only. Deploy→verify is broken for agents *and* silently degrades the GitHub Action's post-deploy polling.
- **`l8b login` is TTY-interactive** (dialoguer/rpassword) — an agent deadlocks. Token minting is dashboard-only.
- **No machine-readable output.** No `--json` anywhere; all output is human prose with ANSI colors. Exit codes don't answer "is it up?" (`status` returns 0 regardless of project state).
- **Runtime env is a file you SSH to edit** (`litebin/projects/<id>/.env` on the node). Unreachable for agents; the first real app deploy ends with "please SSH in."
- **No agent packaging.** No MCP server, no `l8b.toml`, no pairing flow. `llms.txt` exists but is API-shaped and drifting from the CLI.
- **Docs bug:** `api-reference.md` claims `GET /projects` is public; it's behind session auth in `app.rs`.

---

## Design Decisions

### 1. Token scope ladder (server)

Not RBAC, not zero control. One fixed, cumulative ladder + the project binding that already exists.

- Migration: `deploy_tokens.scope TEXT NOT NULL DEFAULT 'deploy'` — values `read | deploy | manage | admin`, cumulative (`deploy ⊇ read`, etc.). Existing tokens become `deploy` (gaining `read` — the intended fix for the broken verify loop).
- `project_id` binding (nullable = global) rides the existing column: a per-project `manage` token is the default for the agent-in-a-repo case.
- Single level per token (radio choice), not a permission set — trivially migratable later if a set is ever needed.

| Level | Cumulatively allows | Endpoints (split from today's session-only group in `app.rs`) |
|---|---|---|
| `read` | See state | `GET /projects`, `GET /projects/:id`, `/stats`, `/logs`, `/deploy-logs`, `/disk-usage`, `GET /nodes`, `GET /meta` (new) |
| `deploy` | read + ship code | `/deploy` POST/PUT, `/deploy/compose`, `/compose/validate`, `/images/*` |
| `manage` | deploy + operate apps | stop/start/recreate, service start/stop/restart, project+service settings (incl. `custom_domain`), capabilities, routes, **env API** |
| `admin` | manage + destructive/platform | project delete, volume deletes, node lifecycle, token CRUD, global settings/domain ops |

Rules:

- `GET /settings` stays session/admin-only (it likely carries Cloudflare tokens — verify). Add lightweight `GET /meta` (read level) → `{domain, dashboard_subdomain, version}` for CLI URL computation.
- `admin` tokens only mintable from a session (dashboard or device-flow approval with explicit warning), never from a lower token.
- Data-loss and platform ops are admin-only: an agent can operate apps, not reconfigure or destroy the platform.
- Session auth continues to pass everything (back-compat for humans/dashboard).

### 2. Machine output contract (CLI)

- Global `--json` flag (+ `L8B_JSON=1`): one JSON object on stdout, progress chatter to stderr, secrets never serialized.
- Success: `{"ok": true, ...payload}`. Failure: `{"ok": false, "error": {"message": "...", "hint": "<exact next command/flag>"}}`, exit 1. The `hint` field is where guided recovery lives (e.g. missing capability → the exact `--grant-capability` flag).
- `status --wait` exits 0 **only** when the project reaches running — the definitive "is it up?" answer. `--healthy` probes the live URL (wake-tolerant timeout) and reports `http_status` + `latency_ms`.
- Status/deploy results become typed `Serialize` structs rendered as either JSON or the current human view. **These same structs are the future MCP tool results** — build them once.

### 3. Secret management (env API)

**No external secret manager as a dependency.** The industry pattern at this scale (Dokku, Docker Compose `env_file`, Vercel, Fly `secrets set`, Coolify) is: authenticated TLS API → store on node → inject at container start. LiteBin already has the protected transport (Caddy TLS client-side, mTLS orchestrator↔agent) and file-on-node storage (never DB).

What separates secure from naive is semantics, not transport:

| Rule | Why |
|---|---|
| Write-only API — **no plaintext read-back endpoint, ever** | A read-back endpoint turns every leaked token into a full secret dump. Recovery = SSH to your own file. |
| Listings show keys + masked previews only (`k•••`, length) | Agents/dashboards see *what* is configured, never values. |
| Env write requires `manage` scope | Not `read`, not `deploy`. |
| Audit without values — log who set which keys when, never values | Extends the existing `last_used_at` pattern. |
| `0600`, service-user-owned files; values never in `--json`, logs, or errors | Agent-facing surfaces are the realistic leak vectors. |
| File/stdin input only (`l8b env push --file`, `--stdin`) | `KEY=VAL` args leak via shell history and agent transcripts. Agents touch paths, never values. |

Threat model (document this):

- **Defensible:** transit, read-back, scope abuse, filesystem perms, audit.
- **Not defensible by anyone:** root on the node (reads `/proc/<pid>/environ` of containers regardless of storage design). Encrypted-at-rest with a local key protects only disk backups — optional hardening later, not phase 1.

Escape hatches that compose:

- **SOPS + age (day one, zero code):** user commits encrypted env to the repo; agent runs `sops -d | l8b env push --stdin`. Plaintext never touches git or the agent's context. Document as *the* recommended agent workflow.
- **External-manager references (later):** `DATABASE_URL=vault://secret/db#url` / `doppler://…` resolved by the node at deploy time (Kubernetes ExternalSecret pattern). Optional, additive.

Implementation shape: `GET/PUT /projects/:id/env` on orchestrator (manage scope); single-node writes the local file directly, remote nodes go through a new agent endpoint — the scan/import path (`ComposeFileResponse { compose_yaml, env_content }` in `litebin-common/src/agent_api.rs`) already established the DTO + `AgentClient` pattern. No new apply mechanism: `recreate` already picks up `.env` changes.

### 4. Agent bootstrap: device pairing flow (phase 2)

`l8b login` becomes the GitHub-CLI-style device flow: CLI gets a short-lived pairing code (low-privilege, single-purpose, safe to print in chat) → prints `Visit https://l8bin.example.com/connect and enter code L8B-4KXQ` → user's browser already has the dashboard session → approve page shows scope radio + project binding → CLI polls, receives token, stores it locally.

- The agent's whole job: run one command, relay one URL. No passwords in terminals, no secrets in chat.
- Server: one small table + 3 endpoints + one dashboard page. CI stays `L8B_SERVER` + `L8B_TOKEN` env vars.
- Long-term: OS keyring storage instead of plaintext `session.json` (phase 4).

### 5. MCP server (phase 3) — nothing runs "somewhere"

**stdio transport**: the coding agent spawns `l8b mcp` as a child process per session; no port, no daemon, no infrastructure. It reads the **same stored credential** as the CLI — the agent config contains no secrets:

```json
{ "mcpServers": { "litebin": { "command": "l8b", "args": ["mcp"] } } }
```

Tools map 1:1 to commands (`deploy`, `status --wait`, `logs`, `restart`, `env push`, `url`, `list`), returning the phase-1 JSON structs — which is why MCP comes last: by then it's a thin wrapper. Later (phase 4, optional): the orchestrator itself can expose MCP over streamable HTTP at `/mcp` for web-based agents with no local CLI.

### 6. CLI scope vs the roadmap's "CLI Scope (Deferred Decision)"

`roadmap.md` caps the CLI at ~12-15 commands (plus an `l8b api` escape hatch) on the premise that "most users use the dashboard; CLI is for CI/CD." The AI-native plan **inverts that premise**: the coding agent becomes a primary CLI consumer, and for an agent, well-named verbs with consistent `--json`, scope-aware errors, and `hint` fields beat a raw API wrapper (discoverability, safe argument surface, no endpoint memorization). Notably, the roadmap's deferred list already included `env set` and `logs`.

Resolution: the phase-1 verb set (`list`, `logs`, `stop`, `start`, `restart`, `url`, `delete`, `env`) is agent-driven priority, not scope creep. The `l8b api get/post/put/delete` wrapper remains complementary as the long-tail escape hatch (and a convenient MCP tool backing), and dashboard-stays-primary for human config surfaces still holds.

### 7. Project config: `l8b.toml` (phase 2)

Checked into the repo: project id, port, node, env-file mapping. This is how any agent opening a repo *discovers* "this deploys via LiteBin" without being told, and how the paired token gets bound to exactly that project. Written by `l8b init`.

---

## Phased Plan

### Phase 1 — Agent-ready core

With a dashboard-minted token in `L8B_TOKEN`, an agent gets the complete deploy→verify→troubleshoot→fix-env→restart loop with structured output. One dashboard visit ever (to mint the token) remains until phase 2.

| Slice | Contents | Touches |
|---|---|---|
| **1. Scope ladder** | Migration (`scope` column), scope enum + rank check, split session-only routes into read/manage/admin groups (session-or-token), `GET /meta`, `POST /deploy-tokens` accepts scope, 401/403/200 test matrix per group | `app.rs`, `auth/mod.rs`, migration, `deploy_tokens.rs`, `routes/meta.rs` (new) |
| **2. Env API** | `GET/PUT /projects/:id/env` (write-only semantics, masked list, audit-without-values), agent-side env endpoint for remote nodes, `l8b env push/set/list` (file/stdin only, masking) | `routes/env.rs` (new), `agent_api.rs`, agent route, CLI command |
| **3. Output contract** | Global `--json` + `L8B_JSON`, `{ok:…}` shapes with `hint`, typed result structs, `status --wait/--timeout/--healthy` | `main.rs`, `out.rs` (new), `status.rs` |
| **4. New verbs** | `l8b list`, `l8b logs <project> [--tail N] [--deploy]`, `l8b stop/start`, `l8b restart` (single-service → recreate; compose → per-service), `l8b url`, `l8b delete --yes` (admin). All `--json`-aware, token-or-session | `commands/*` (new files), `auth.rs` bearer-capable helpers |
| **5. Docs sync** | Fix `api-reference.md` auth columns (incl. the "public" bug), document scopes + new commands, regenerate `llms.txt` CLI section via `--generate-markdown` | `docs/` |

**Acceptance check (phase 1):** agent with a token deploys an app that needs `DATABASE_URL` → app crashes → reads `logs` → pushes env via `l8b env push` → `restart` → `status --wait --healthy` returns running + `http_status: 200`. No SSH, no dashboard, secrets never printed. Human-mode output for existing commands stays byte-compatible. Old deploy tokens gain `read` (intended). GitHub Action post-deploy polling starts working under token auth.

### Phase 2 — Zero-dashboard bootstrap + env surface completion

- Device pairing flow (`l8b login` rework, `/auth/device/*` endpoints, dashboard connect page with scope radio + project binding).
- `l8b init` + `l8b.toml` (project discovery; optionally emit workspace `.mcp.json`).
- Assisted setup: `l8b doctor` (server reachable, auth valid, Docker present, DNS preflight — with exact next steps), `l8b setup` (register first admin + mint/pair token against a fresh server), non-interactive installer flags (agent *can* drive install if the user opts in — trust boundary stays human).
- `l8b domain set` command.

**Env everywhere** (closes the runtime-env matrix; every surface can set env at deploy time or after):

| Surface | Deploy-time env | Post-deploy env |
|---|---|---|
| `l8b deploy` | `--env-file <path>` — pushes after staging, before first start (new projects stage first via `stage_only`, then env, then start) | `l8b env push` ✓ done |
| `l8b ship` | The "Awaiting runtime configuration" pause becomes a real step: pick local `.env` file(s) → push via API → start (skip = start with defaults) | same pause on resume ✓ |
| Dashboard deploy dialog | Optional env textarea (KEY=VALUE); staged projects get env → start instead of start-then-pending | — |
| Dashboard project view | — | Env modal: masked key list + `pending_apply` indicator + editor dialog (write-only PUT); unconfigured projects get env → start in-page (replacing the current "resume from the CLI" dead end) |
| Direct file edit | ✓ unchanged (same file both paths write) | ✓ unchanged |

Dashboard work (connect page + env modal + deploy dialog) lands as one React pass.

**Deferred to end of phase (agreed):** auth-endpoint rate limiting (per-IP token
bucket on `/auth/login`, `/auth/device/start`, `/auth/device/token`) + pending-code
cap + connect-page "only approve codes you initiated" copy. Guessing is not the
risk (122-bit device_code, 256-bit tokens); brute-force and spam are. Details in
[security-hardening.md](security-hardening.md) §9.

### Phase 3 — Native integration

- `l8b mcp` stdio server wrapping the same client code; tools return phase-1 result structs.
- AGENTS.md / skill templates so "deploy using litebin" triggers reliably in any agent.
- Refresh `llms.txt` to be task-oriented ("is my app up?" → `l8b status --p x --wait --healthy`).

### Phase 4 — Hardening & polish

- OS keyring for stored credentials; optional encryption-at-rest for node env files (key from env var, decrypt in-process at start).
- `logs -f` streaming; remaining verbs (`volumes`, `routes`, `nodes`, `tokens`).
- External secret-manager references (`vault://`, `doppler://`) resolved at deploy time.
- Optional HTTP MCP endpoint hosted by the orchestrator for zero-install web agents.

---

## Verify During Implementation (not yet confirmed)

1. **`GET /settings` contents** — if it returns Cloudflare API tokens, the `GET /meta` split is mandatory; if not, `read` on `GET /settings` is the smaller change. Planned for the safe version regardless.
2. **axum-login behavior for Bearer requests** — confirm 401 JSON (not a 302 redirect) so CLI/agent error handling is clean.
3. **`.env.l8bin` snapshot** — how the post-start env snapshot interacts with the env API (don't let writes race container starts; snapshot should reflect what was actually injected).
4. **Compose restart semantics** — pick `recreate` vs per-service `restart` for `l8b restart` on multi-service projects (data volumes must be preserved either way — they are, volumes are separate from containers).
5. **Deploy timeout exit code** — decide: `deploy --json` when still `deploying` at timeout → exit 0 + `"status": "deploying"` (agents read the field) vs a distinct exit code. Lean: exit 0 + field, keep exit codes for transport/command failures only.
