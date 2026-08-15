# Hot Function Extraction

## Context

The file-size refactor left exactly two oversized functions — the two highest-risk units in the codebase, both requiring ~400 lines of context to change safely:

| Function | Location | Size | Why it's risky |
|---|---|---|---|
| `run_service_container` | `litebin-common/src/docker/container/run.rs` | ~390 lines | Interleaves port bindings, raw-ports policy, socket sanitization, security overrides, compose-vs-single create paths, startup stabilization, mapped-port resolution — all in one body with one shared closure |
| `start_services` level loop | `orchestrator/src/routes/manage/multi_service/start.rs` | ~380 lines | Level-by-level JoinSet scheduling, per-service fast-path/recreate task, proxy loopback verification, three near-identical rollback blocks, DOCKER_HOST rewrite |

Unlike the round-2 extractions (which moved inline blocks of *sequential* handler code), these are stateful: the loop bodies mutate shared Arc<Mutex<...>> trackers and the closure captures seven locals. Extraction means threading that state through a small context struct per function.

## Target Pattern

For `run_service_container`: split along its natural seams into helpers on `DockerManager` or free fns in `run.rs` —
- `compute_port_bindings(&RunServiceConfig) -> Result<PortPlan>` (public port, proxy loopback, raw ports, reserved-port policy)
- `apply_security_overrides(host: &mut HostConfig, limits: ResourceLimits)`
- `build_compose_create_body(...)` / `build_single_create_body(...)` (the two paths share only the overrides)
- `resolve_mapped_port(...)` (the non-fatal inspect-and-warn tail)

For `start_services`: extract the per-service task closure into `start_one_service(...)`, and unify the three rollback blocks (proxy-inspect failure, task failure, panic) into one `rollback_and_mark(...)` helper parameterized by whether to stop containers first — they are currently copy-pasted with tiny differences, which is itself a latent bug surface.

## Considerations

- Behavior-preserving, but concurrency code — do it under the existing test suite plus the agent live tests (`cargo test -p litebin-agent -- --ignored`) run before and after.
- The rollback unification in `start_services` is the highest-value piece alone: today the three copies have already drifted subtly (one logs "rollback: stopped after failure", two don't).
- Do the two functions as separate efforts; `run_service_container` first (fewer moving parts, pure construction logic dominates).

## Priority

Medium — no current pain, but every future feature touching ports, security policy, or orchestration lands in these two bodies.
