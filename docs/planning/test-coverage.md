# Test Coverage Expansion (Agent + CLI)

## Context

The orchestrator has a real integration suite (81 tests, in-memory SQLite + real migrations). The other crates are nearly untested:

| Crate | Tests | State |
|---|---|---|
| orchestrator | 81 | Integration + unit; healthy |
| compose-bollard | 51 | Unit + proptest; healthy |
| litebin-common | 49 | Unit + proptest; healthy |
| agent | 4 | 1 unit + 3 `#[ignore]`d live-Docker tests |
| cli | 4 | Ad hoc |

The agent's deploy path (`batch_run/prepare.rs`, `execute.rs`) contains the highest-risk pure logic in that crate — proxy reuse/replacement decisions, host-network authorization, partial-redeploy targeting, resource-override application — with zero unit coverage; the only tests need a real Docker engine. The CLI ships easily-testable pure logic (public-service candidates, env precedence, port parsing) untested, and its interactive flows have no seams.

## Current Pattern

Live-only testing: the agent's decision logic is only exercised through `#[ignore]`d end-to-end Docker tests; nothing runs in CI.

## Target Pattern

- Extract pure decision functions where they're entangled with I/O (most already are pure after the batch_run module split).
- Unit-test them against fixtures: proxy reuse matrix (granted/not-granted × host-observers × partial/full), host-network gate, target-set expansion, resource override precedence (compose < dashboard override < global default).
- CLI: unit tests for `public_service_candidates`, `pick/auto_pick_public_service` (via YAML fixtures), `env_precedence_score`/`discover_env_files` ordering, `findings` grouping.
- Add `cargo-llvm-cov` as a documented local tool (not a CI gate initially) so gaps are visible: `cargo llvm-cov --workspace --ignore-filename-regex 'tests\.rs|main\.rs'`.

## Scope

| Area | Files | Tests to add (est.) |
|---|---|---|
| Agent plan decisions | `agent/src/routes/containers/batch_run/prepare.rs` | 8–12 unit tests |
| Agent rollback helpers | `batch_run/rollback.rs`, `types.rs` | 2–3 |
| Agent metadata/scan handlers | `metadata.rs`, `scan.rs` | 3–4 |
| CLI ship pure logic | `cli/src/ship/public_service.rs`, `env.rs`, `ui.rs` | 8–10 |
| CLI build context guard | `cli/src/build/context.rs` (uses tempdirs) | 2–3 |

## Considerations

- The `prepare.rs` phases take `&AgentState` — decide between thin trait seams (trait DockerOps) or refactoring the pure parts to take plan values only. Prefer the latter; avoid mocking Docker.
- Live tests stay as-is (`--ignored`, local-only, documented in development.md); they remain the end-to-end check.
- Coverage number is directional only — don't gate CI on a %.

## Priority

Medium-high — the agent is the least-tested crate despite owning container lifecycle; the batch_run split made this cheap to fix.
