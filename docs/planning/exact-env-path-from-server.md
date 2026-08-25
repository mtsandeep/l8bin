# CLI: exact `.env` path from the server instead of a guess

**Status:** planned, not implemented. Revisit later.
**Scope:** `cli` (`ship/ui.rs`), `orchestrator` (projects route + `AgentClient`), `agent` (internal route), `litebin-common` (`types.rs`, `agent_api.rs`), `install.sh`.
**Related:** `docs/env-secrets.md` (runtime secrets layout), `docs/agent-api.md` (typed wire contract).

## Problem

`l8b ship` pauses at "Awaiting runtime configuration" and prints where the runtime `.env` lives. `show_env_path` in `cli/src/ship/ui.rs` *guesses* — two candidate paths plus a caveat:

```
🔒 Runtime secrets on node local: ~/litebin/projects/<id>/.env  or  ./litebin/projects/<id>/.env
     (default install path; if custom -InstallDir was used, prepend that path instead)
```

The CLI is already authenticated, so the server can report the exact path and the guess/caveat can go away.

## The wrinkle that shapes the design

Master/agent normally run in Docker and see the in-container path `/app/projects/<id>/.env` (the bind-mount target — see `projects_dir()` in `litebin-common/src/types.rs` and install.sh's `-v …/projects:/app/projects`). The path the *user* needs is the **host** side of that bind. Only the installer knows it, so it must pass it in. Binaries running directly on the host (dev, Windows, manual installs — e.g. `C:\Users\<user>\litebin`) can simply canonicalize their real path.

## Plan

### 1. litebin-common — shared path resolution + wire type

- In `types.rs`, next to `projects_dir()`: add `host_projects_dir() -> Option<PathBuf>`:
  - `L8B_PROJECTS_HOST_DIR` env set & non-empty → use it (installer-provided host path).
  - else `projects_dir()` == `/app/projects` (in Docker, no override) → `None` (host path unknowable from inside the container).
  - else canonicalize; on Windows strip the `\\?\` verbatim prefix so it prints cleanly.
- In `agent_api.rs`: `pub const ENV_PATH_PATH: &str = "/internal/env-path";` + `EnvPathResponse { projects_dir: Option<String> }` (`skip_serializing_if` None), matching the existing typed-contract style.

### 2. Agent — report its own dir

- New tiny handler `agent/src/routes/env_path.rs`: `GET /internal/env-path` → `EnvPathResponse`.
- Register in `agent/src/lib.rs` next to the other `/internal/*` routes (same mTLS-protected router — no extra auth needed).

### 3. Orchestrator — project-scoped endpoint

- `AgentClient::env_path()` in `orchestrator/src/nodes/client.rs` using the existing `get_json` helper.
- New handler `GET /projects/{id}/env-path` (in `routes/projects`, modeled on `get_project`):
  - Load project → 404 if missing; `node_id = project.node_id.unwrap_or("local")`.
  - Local: `env_path = host_projects_dir().map(|d| d.join(&id).join(".env"))`.
  - Remote: `AgentClient::resolve(...)` → `env_path()`; on any agent error return `env_path: null` (don't fail the endpoint — CLI falls back).
  - Response `{ "node_id": String, "env_path": Option<String> }`.
- Register in `app.rs` in the session-protected `api_routes`. Follow the existing utoipa annotation pattern if these routes are registered in openapi.rs.
- Read the env var via the shared helper (not via `Config`) so master and agent share one code path.

### 4. CLI — fetch, print exactly, keep fallback

- `show_env_path` → `async fn show_env_path(client, server, project_id, node_id)`:
  - `auth::session_get(client, server, "/projects/{id}/env-path").await.ok()` → read `env_path`.
  - Known → print one exact path (no "or", no caveat line); keep the remote-node "edit on the agent" hint.
  - Unknown → current heuristic output unchanged.
- Update the 3 call sites to await: `flow.rs` (`await_runtime_config_and_start`) and `deploy.rs` ×2 (`finish_deploy_response`) — both already have `client`, `server`, node id in scope.

### 5. install.sh — supply the host path

- Master `.env` generation (~line 677): add `L8B_PROJECTS_HOST_DIR=<absolute ${install_dir}/projects>` (absolutize install_dir first).
- Agent `.env` generation (~line 1237): same variable; agent container gets it via `--env-file` (recreate needed to pick it up).
- Existing-install upgrade: follow the `MASTER_CA_CERT_PATH` append-if-missing pattern (~line 1014) to append the var to both master and agent env files, with an info line telling the user to restart/recreate containers.

### 6. Docs + changelog

- `docs/env-secrets.md`: CLI now prints the exact path; document `L8B_PROJECTS_HOST_DIR`.
- `CHANGELOG.md` Unreleased → Added.

## Why a separate endpoint (not piggyback on deploy responses)

The env-path hint is shown from several flows (first deploy, redeploy, "Resume deployment"), and response structs differ per path. A small GET endpoint serves all of them, degrades cleanly on old servers (404 → CLI falls back), and old CLIs simply never call it.

## Verification

- `cargo clippy --all-targets -D warnings` + `cargo test --workspace` (CI gate parity).
- New tests: `host_projects_dir()` env-override unit test in litebin-common; orchestrator route test (local node → `env_path` ends with `projects/<id>/.env`, `node_id == "local"`) using the existing `tests/helpers.rs` router pattern.
- Manual: `l8b ship` against a local Windows master → prints a single exact `C:\Users\<user>\litebin\projects\<id>\.env` line, no "or"/caveat.

## Back-compat

- New CLI + old server → 404 → heuristic (unchanged behavior).
- Old CLI + new server → endpoint simply unused.
- Existing Docker installs until re-patched → `env_path: null` → heuristic.
- Remote node offline → null → heuristic for that deploy.
