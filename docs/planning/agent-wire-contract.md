# Typed Orchestrator ↔ Agent Wire Contract

## Context

Every orchestrator → agent HTTP call builds its JSON body with the `json!` macro (~250 uses workspace-wide) and the agent parses it into structs on the other side. Nothing links the two sides: renaming a request field on the agent compiles fine on the orchestrator and fails at runtime, in production. The same agent-call boilerplate (get_node_client → get_node_from_db → agent_base_url → client.post) is repeated ~9 times, and the batch-run payload construction (service_resources query, default memory/CPU settings reads, capability lookups) is copy-pasted four times (`handlers/stop_start.rs`, `handlers/recreate.rs`, `deploy/compose/remote.rs`, `deploy/single/core.rs`).

This is the largest remaining source of silent contract drift and duplicated logic.

## Current Pattern (to replace)

```rust
// orchestrator — hand-built, untyped
let resp = client
    .post(format!("{}/containers/batch-run", base_url))
    .json(&json!({
        "project_id": &project_id,
        "compose_yaml": &compose_yaml,
        "service_order": &svc_names,
        // ... 10 more fields, repeated in 4 places
    }))
    .send()
    .await?;

// agent — typed struct that must be kept in sync by discipline
pub struct BatchRunRequest { ... }
```

## Target Pattern

```rust
// litebin-common: single source of truth for the protocol
pub mod agent_api {
    pub struct BatchRunRequest { pub project_id: String, /* ... */ }
    pub struct BatchRunResponse { pub services: Vec<ServiceRunResult>, pub warnings: Vec<String> }
    // ... one struct per endpoint, used by BOTH crates
}

// orchestrator — typed client wrapper
let agent = AgentClient::from_state(&state, &node_id).await?;
let result = agent.batch_run(agent_api::BatchRunRequest { ... }).await?;
```

## Scope

| Piece | Where | Notes |
|---|---|---|
| `agent_api` DTO module | `litebin-common/src/` | One struct per internal endpoint (run, batch-run, stop-service, stop-project, cleanup, register, project-meta, caddy/sync, compose-file) |
| `AgentClient` wrapper | `orchestrator/src/nodes/` | Holds client + base_url; one method per endpoint; shared reqwest::Client (replaces 9 ad-hoc constructions); centralizes unreachable/non-success error mapping |
| `build_batch_run_payload` | `orchestrator/src/routes/manage/` | One implementation of the resource-override + defaults + capability reads; deletes 3 copies |
| Agent-side adoption | `agent/src/routes/` | Handlers accept the shared DTOs instead of local duplicates (removes agent's local `BatchRunRequest` etc.) |

## Considerations

- Agent already has local DTOs that mirror the wire shape — moving them to litebin-common is mostly a re-parent, plus the `ErrorResponse` duplication.
- The orchestrator's `ServiceInfo` move (phase 4 refactor) is the precedent; this completes it for the internal protocol.
- `ClientError` for the HTTP layer belongs with `docs/planning/error-types.md` — this doc depends on that (or inlines a minimal error type first).
- Protocol versioning already exists (`protocol_version` in HealthReport); shared DTOs make future version bumps explicit at compile time.
- Do endpoints gradually (batch-run first — highest duplication), not big-bang.

## Priority

High — largest remaining cross-crate correctness and duplication win; de-risks every future agent API change.
