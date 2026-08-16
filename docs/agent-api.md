# Agent API (Typed Wire Contract)

The orchestrator ↔ agent internal protocol is defined once, in code, and shared by both sides: every request/response body is a struct in [`litebin-common/src/agent_api.rs`](../litebin-common/src/agent_api.rs), and the orchestrator sends/receives them through the typed [`AgentClient`](../orchestrator/src/nodes/client.rs). Renaming a field breaks compilation on the other side instead of failing at runtime in production.

---

## Layout

| Piece | Where | Role |
|---|---|---|
| DTOs + endpoint path constants | `litebin-common/src/agent_api.rs` | One struct per internal endpoint, used by BOTH crates; wire-format tests pin the serialized field names |
| `AgentClient` + `AgentClientError` | `orchestrator/src/nodes/client.rs` | One typed method per endpoint; centralizes URL building and the 503/502/500 error convention |
| `build_batch_run_payload` | `orchestrator/src/routes/manage/multi_service/helpers.rs` | Single implementation of the batch-run payload (service resource overrides + global defaults read once), shared by start/recreate/deploy/stage |
| Agent handlers | `agent/src/routes/*` | Extract/respond with the shared DTOs directly (local struct definitions re-export from `agent_api`) |

`AgentClient::resolve(state, node_id)` performs the DB lookup + client pool lookup + base-URL derivation that was previously repeated at every call site. The `AppState.node_clients` pool type is unchanged.

## Error convention

`AgentClientError` maps to the handler convention:

| Variant | Meaning | Handler status |
|---|---|---|
| `Transport` | agent unreachable (connection/timeout) | 503 |
| `Status { code, body }` | agent answered non-success; `body` is the agent's `ErrorResponse`/`BatchRunErrorResponse` text | 502 |
| `Parse` | agent response didn't match the DTO | 500 |

Call sites with special semantics match variants directly (e.g. reconciliation treats `Status` with 404 as "container gone"; the waker falls back to recreate when `/containers/start` fails).

## Compatibility rules

- Field names and serde attributes in `agent_api` **are** the wire format — don't change them without a `protocol_version` bump (see `types::HealthReport`).
- Requests: `Option` fields serialize as `null` when `None`; the agent's deserialization treats absent and `null` identically, so payload shapes are compatible with the pre-contract `json!` bodies.
- Responses: fields with `skip_serializing_if` also carry `serde(default)` so clients tolerate their absence.
- The unit tests at the bottom of `agent_api.rs` pin the serialized shapes for the high-traffic endpoints; keep them passing.

## Out of scope (deliberately)

- `/caddy/sync` request bodies stay raw `serde_json::Value` — the Caddy config is dynamic JSON by design; only the transport is typed.
- The waker's HTTP proxy path and the upload loopback routes (`/__l8b_upload/*`) are not JSON DTO endpoints.
