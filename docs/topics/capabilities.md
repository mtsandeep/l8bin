# Project Capabilities

Some compose features need privileges beyond LiteBin's default sandbox. Those features *request* a capability; only you can *grant* it. This page is the full reference — the dashboard and CLI show abbreviated versions.

---

## Model

| | |
|---|---|
| **Requested** | A compose declaration implies a privilege (e.g. an explicit `HOST:CONTAINER` port mapping requests `raw-ports`). Visible in the Capabilities tab as "Needed". |
| **Granted** | You approved it for the project. Grants persist per project and survive redeploys. |
| **Deferred** | You deployed without deciding: everything is uploaded and staged, but the project **cannot start** until you grant the capability. |
| **Revoked** | You removed the grant. Takes effect on the next container recreate; see per-capability notes below. |

Starting a project whose compose requires an un-granted capability is refused with a clear error — LiteBin never silently runs a degraded version (dropping host ports or stripping the Docker socket) of a compose that asked for them.

All capability changes take effect on the next container **recreate**. Grants and revocations sync to agents immediately, but port bindings and network modes are set at container creation.

---

## Reference

### Raw host ports (`raw-ports`)

Publishes explicit `HOST:CONTAINER` Compose ports directly on the host (`0.0.0.0`). Required for non-HTTP services that external clients must reach on a real node port — databases (`"3306:3306"`), game servers (`"25565:25565/udp"`).

- **Risk:** medium — opens host ports and can conflict with other services; [LiteBin-reserved ports](faq.md#litebin-reserved-ports-even-with-allow-raw-ports) are always blocked.
- **Avoid it when you can:** services inside the stack reach each other over the managed network without any mapping. Container-only syntax (`"8080"`) never publishes and never needs the capability. The deploy prompt can rewrite your mappings for you ("Remove host ports"; CI: `--strip-host-ports"), and the Settings → Capabilities tab offers the same fix on the raw-ports card for an already-deployed project — see [Why is my deploy asking for raw-ports?](faq.md#why-is-my-deploy-asking-for-raw-ports-host-ports).
- **Revoke:** running containers keep their existing bindings until the next recreate, which then requires the capability again — a revoked-and-stopped project will not start until re-granted.

### Docker observation (`docker-observe`)

Allows selected services to read host-wide container information through LiteBin's endpoint-allowlisted proxy. The raw Docker socket is never mounted into the workload — declares of `/var/run/docker.sock` are rewritten to the proxy, and without the grant the socket declaration is removed entirely (fail closed).

- **Risk:** high — responses may expose host-wide container metadata, environment values, and logs.
- **Revoke:** the observation proxy container is removed immediately; the workload's Docker API access dies with it.

### Host networking (`host-network`)

Runs approved background services in the host network namespace. The service can access host-bound listeners and exposes its own listeners directly. Restricted to **background projects** (no managed HTTP ingress); requires a rootful Docker engine and cannot be combined with Compose `ports` or custom networks.

- **Risk:** high — removes network namespace isolation and may expose host services or create listener conflicts.
- **Revoke:** all project containers are stopped — a host-network container cannot keep running without the grant.

---

## Where grants are enforced

| Checkpoint | Behavior |
|---|---|
| Deploy | Required-but-ungranted capabilities block the deploy unless granted, deferred, or resolved (e.g. host ports removed). |
| Start (`l8b start`, dashboard, resume) | Refused with an actionable error while any requirement is ungranted. |
| Auto-wake (URL request to a stopped project) | Refused instead of silently starting without the capability. |
| Agent batch-run | Host-network requests are re-checked against the orchestrator's flags. |

## API and CLI

- Dashboard: project → Settings → Capabilities (grant/revoke per capability; "Remove host ports" on the raw-ports card).
- CLI deploy: `--grant-capability <id>`, `--defer-capability <id>`, `--strip-host-ports`.
- REST: `GET/POST /projects/{id}/capabilities`, `DELETE /projects/{id}/capabilities/{capability}`, `POST /projects/{id}/capabilities/strip-host-ports`.
