# API Reference

## Orchestrator API

Base URL: `https://l8bin.example.com` (or `https://l8bin.localhost` locally)

**The endpoint-by-endpoint reference lives in the generated OpenAPI spec** — every operation with its parameters, auth, and response schemas:

- **Interactive docs**: `/docs` on your server (Scalar UI), rendered from `/openapi.json`
- **Raw spec**: [`openapi.json`](../openapi.json) (OpenAPI 3.1, regenerated from source on every release via utoipa — always current)
- Feature docs add context per area: [backup.md](../topics/backup.md), [github-connect.md](../topics/github-connect.md), [ai-native.md](../topics/ai-native.md)

This page covers what the spec can't: how authentication and scopes work.

### Authentication

Two methods:

- **Session cookie** (dashboard, `l8b login`) — full access to everything.
- **Deploy token** (`Authorization: Bearer <token>`) — scoped, cumulative access levels: `read < deploy < manage < admin`.

| Scope | Cumulatively allows |
|---|---|
| `read` | See state: project list/get, stats, logs, deploy-logs, disk-usage, node list, `/meta`, `/whoami` |
| `deploy` | read + ship code: `/deploy`, `/deploy/compose`, `/compose/validate`, `/images/*` |
| `manage` | deploy + operate apps: stop/start/recreate, service ops, project/service settings, routes, capabilities, env writes |
| `admin` | manage + destructive/platform: project delete, volume deletes, node lifecycle, token CRUD, global settings |

Notes:

- Tokens are global or bound to a single project (`project_id`); project-bound tokens can only access that project's `/projects/{id}/…` paths (plus `/meta` and `/whoami`).
- `admin` tokens are mintable only from a session (`POST /deploy-tokens` is session-only).
- Read-only tokens cannot deploy. Unauthenticated calls return `401` JSON.
- Every response is JSON; errors are `{"error": "…"}` with a fitting HTTP status.

---

## Agent API

The orchestrator ↔ agent protocol is a typed wire contract (`litebin-common/src/agent_api.rs`) — not part of the public HTTP API. See the "Agent API (Typed Wire Contract)" section in [development.md](../dev/development.md).
