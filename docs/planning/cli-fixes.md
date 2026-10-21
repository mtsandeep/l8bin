# CLI fixes

Deferred fixes from the 2026-10-21 command audit. Nothing here is broken today — the list is ordered by user impact; take items up independently.

**Last updated: 2026-10-21**

---

## High

- **`logs --follow`** — `l8b logs` has only `--tail/--service/--deploy`; no streaming. Every deploy CLI follows logs; add `-f/--follow` (server has a logs endpoint already — verify streaming/SSE support in `orchestrator/src/routes/stats.rs`).
- **`restart` recreates, doesn't restart** — it POSTs `/projects/{id}/recreate` (cli/src/commands/projects.rs:288) while the ship action menu calls the same thing "Recreate". Add a `recreate` command and keep `restart` as an alias — or split semantics if a true stop+start restart is ever wanted.
- **`login`/`setup` should prompt for the server** — both hard-require `--server` (clap `String`), so bare `l8b login` dies with a usage error. `ship` already prompts interactively (main.rs ship arm); make these consistent.

## Medium

- **`whoami` command** — surface `GET /whoami` (kind, scope, project binding) so users can ask "what can this token do". Ship already consumes it internally (`bound_token` in cli/src/ship/flow.rs).
- **Shell completions** — `l8b completions bash|zsh|fish` via `clap_complete`.
- **`config set` repurpose** — today it means "add a credential headlessly" (requires both `--server` and `--token`, main.rs `ConfigAction::Set`). Either rename it, or make the store's `default` field real: a `l8b use <server>` global default that `pick_server` consults (currently written on every login, never read).

## Low

- **`cleanup`** — takes a positional path while every other command uses `--path`; also niche enough to fold into `doctor`.
- **Flag consistency** — `delete --yes` lacks `-y`; only `status --project` has `-p`. Pick one convention and apply it.
- **Stale doc comment** — projects.rs `domain_set` says `domain set <project> <domain>`; actual shape is `domain set <domain> -p <project>`.
- **`list` filters** — `--status` / `--node` once project counts grow past a screen.
