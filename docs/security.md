# Security

## Threat Model

LiteBin runs many app containers on shared infrastructure. Apps are treated as untrusted — they may be compromised via CVEs, supply-chain attacks, or misconfiguration. Each app must be isolated from other apps and from the host. The primary threats:

| Threat | Source | Impact |
|---|---|---|
| Container escape | Compromised app container | Access to host, other containers, Docker socket |
| Privilege escalation | Malicious image / app code | Root access within container, potential host access |
| Lateral movement | Compromised app → agent/orchestrator | Control over all deployed apps on the node |
| Network interception | Unencrypted master ↔ agent traffic | Credential theft, MITM attacks |
| Unauthorized deploys | Exposed orchestrator API | Arbitrary container deployment |
| Resource exhaustion | Malicious or buggy app | DoS — consume all host CPU/RAM/disk |

---

## Authentication

**Implementation:** Session-based auth via `axum-login` + `tower-sessions` (SQLite-backed).

| Mechanism | Use Case | Details |
|---|---|---|
| Password login | Dashboard access | `bcrypt` hashing |
| Session cookies | Browser sessions | SQLite-backed, Secure flag in production |
| Deploy tokens | CLI / CI/CD | SHA-256 hashed in DB, scoped per-project or global, optional expiry |

The deploy endpoint supports two-tier auth: session cookie checked first, then `Authorization: Bearer <token>` fallback.

### Password recovery

There is no in-app "forgot password" flow (no email infrastructure is assumed). Operators recover from a forgotten admin password with the out-of-band CLI:

```bash
docker exec -it litebin-orchestrator /app/litebin-orchestrator reset-password
```

Replace `litebin-orchestrator` if your deployment uses a custom container name (the default comes from `container_name` in `docker-compose.yml`).

The command prompts for a username and a new password (hidden input), then writes a fresh bcrypt hash to the `users` table using the same code path as the live `change_password` endpoint.

**Why this is not a security hole:**

- The handler runs as an early branch in `main()` before the HTTP server starts. No route is registered, no socket is opened — it is unreachable from the network.
- Invoking it requires `docker exec` on the orchestrator container, which is the same trust level as direct filesystem/DB access. An attacker with that access can already read or modify the SQLite DB directly.
- Resetting the password also invalidates all existing sessions for that user, because `session_auth_hash` is derived from `password_hash`.

This matches the model used by other self-hosted tools (out-of-band CLI for password recovery).

---

## Network Isolation

All app containers join `litebin-network` for single-service, or per-project `litebin-{project_id}` networks for multi-service.

```
Host
├── litebin-network (management + tenant workloads; per-project litebin-{project_id} for multi-service)
│   ├── orchestrator (5080)
│   ├── dashboard (internal only)
│   ├── caddy (80/443)
│   ├── app-1 (port 50001)
│   └── app-2 (port 50002)
└── agent (5083 → 8443 internal)
```

---

## mTLS (Master ↔ Agent)

Mutual TLS using `rustls` + `WebPkiClientVerifier`. No HTTP fallback — mTLS is mandatory.

- Orchestrator authenticates with `server.pem` / `server-key.pem` when it connects to an agent
- Every agent terminates TLS on port 5083 with the same `agent.pem` / `agent-key.pem` (`CN=agent`, `SAN=DNS:agent`)
- Both sides verify the peer chain against the same Root CA
- Certs are ECDSA P-256 (production installer), valid for 10 years
- Per-node identity for agent → master callbacks uses `agent_secret` (HMAC), not a unique TLS cert

```
Root CA (self-signed, ECDSA P-256)
├── Master identity (server.pem) — orchestrator → agent connections
└── Shared agent identity (agent.pem) — agent API on port 5083 (same on every agent)
```

Production installs distribute the same agent cert bundle to every worker via `bash -s certs`. That keeps setup simple; a compromised agent key can impersonate the shared agent TLS identity (other agent hosts are not automatically owned). Agent → master wake/heartbeat still require that node's unique `agent_secret`.

---

## Container Hardening

| Control | Value |
|---|---|
| `cap_drop` | `ALL` |
| `cap_add` | `CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `FSETID`, `SETGID`, `SETUID`, `NET_BIND_SERVICE`, `KILL` |
| `security_opt` | `no-new-privileges` |
| `pids_limit` | 4096 |
| `log_config` | max-size 10m, max-file 3 |
| `memory` | 256 MiB default, per-project override |
| `nano_cpus` | 0.5 cores default, per-project override |
| `network_mode` | Managed project bridge by default; approved background services may use `host` |
| `restart_policy` | `no` (orchestrator manages lifecycle) |

### Capability strategy

All 14 default Linux capabilities are dropped, then 8 are added back for app compatibility. `no-new-privileges` ensures these cannot be escalated further.

| Capability | Reason |
|---|---|
| `CHOWN` | Frameworks and build tools need to change file ownership |
| `DAC_OVERRIDE` | Apps need to read/write files they don't own |
| `FOWNER` | Required for `chmod`/`chown` on files not owned by the caller (PHP/Apache entrypoints, log rotation) |
| `FSETID` | Required to set the sticky bit on directories (e.g. `chmod 1777 /tmp`) — needed by many image entrypoints during initialization |
| `SETGID` | Required by many runtimes and package managers |
| `SETUID` | Required by many frameworks for user switching |
| `NET_BIND_SERVICE` | Some apps bind to privileged ports |
| `KILL` | Process management within the container |

`FOWNER` and `FSETID` were added because a large class of PHP/Apache-style image entrypoints run `chmod 1777 /tmp` during init, which requires `FSETID`. Neither capability enables container escape or lateral movement — they only affect operations *inside* the container's own filesystem namespace.

Capabilities still blocked (not added back): `CAP_NET_RAW` (no packet sniffing or ARP spoofing of neighbor containers), `CAP_SYS_ADMIN`, `CAP_SYS_PTRACE`, `CAP_SYS_MODULE`, `CAP_NET_ADMIN`, `CAP_DAC_READ_SEARCH`, and others. These are the capabilities that enable container escape or cross-app attack paths.

---

## Agent Security

Master → agent API auth is mTLS (shared agent identity + master identity):

- mTLS is mandatory (agent fails to start without certs)
- All agents use the same CA-signed agent identity on port 5083; peers must present the master identity or the handshake fails
- Agent port (5083) is open to all IPs — mTLS is the auth layer, not firewall rules
- Config is pushed from orchestrator over mTLS via `POST /internal/register` (includes per-node `agent_secret`), no secrets in agent env vars
- Agent → master wake/heartbeat use HMAC-SHA256 with that node's `agent_secret` (see [Wake-Report Endpoint](#wake-report-endpoint))

---

## Docker Socket

Both orchestrator and agent mount `/var/run/docker.sock`. This is required for container management. The risk is mitigated by:

- App containers are on isolated networks with no Docker socket access
- Agent requires mTLS for all connections
- Orchestrator requires session or deploy token auth
- App containers have restricted capabilities

### Project capabilities

Risky workload features require explicit grants stored in `project_capabilities`:

- `docker-observe` — injects a managed HAProxy sidecar with an endpoint-allowlisted, read-only Docker API policy (never raw `docker.sock`)
- `host-network` — runs an approved background service in the host network namespace
- `raw-ports` — publishes Compose ports on the host

Deployments may *request* capabilities; only the user can *grant* them (dashboard validation step, Settings → Capabilities, or `l8b deploy --grant-capability`). `network_mode: host` requires `host-network`; privileged mode remains unsupported.

Docker socket declarations are always removed, including mounts marked `:ro`; filesystem read-only mode does not restrict Docker API operations. With `docker-observe`, HAProxy forwards only `GET`/`HEAD` requests for daemon info, version, events, container listing, container inspect, container stats, and container logs. The requesting service receives `DOCKER_HOST`; other project services do not.

Observation is host-wide. Responses can include container metadata, environment values, and logs from other projects on the node. LiteBin does not expose mutating Docker API access.

Host networking is restricted to background projects using a Linux Docker engine in rootful mode. Compose `ports` and custom networks cannot be combined with it. The workload shares the Docker host network, so it can reach host-bound services and its listeners bind directly on that network. LiteBin's capability drops, `no-new-privileges`, resource limits, and log limits still apply.

When a host-network service also uses `docker-observe`, the HAProxy sidecar remains on an isolated bridge and is published only to a Docker-assigned loopback port. LiteBin injects `DOCKER_HOST=tcp://127.0.0.1:<port>` into that approved service; the proxy is never bound to a non-loopback host address.

---

## Wake-Report Endpoint

Agent → orchestrator wake reports use two security layers:

1. **Poke subdomain** — `poke.{domain}` only proxies `/internal/*` paths, all others return 404
2. **HMAC-SHA256 signing** — requests include `X-Agent-Id`, `X-Agent-Timestamp`, `X-Agent-Signature` headers. Signature is `HMAC-SHA256(agent_secret, timestamp + "\n" + node_id)`. Constant-time comparison, 5-minute replay protection.

---

## Resource Limits

| Resource | Default | Per-project | Config |
|---|---|---|---|
| Memory | 256 MiB | Yes | `memory_limit_mb` in deploy request |
| CPU | 0.5 cores | Yes | `cpu_limit` in deploy request |
| Processes | 4096 | No | `pids_limit` |
| Auto-stop | 15 min idle | Yes | `auto_stop_timeout_mins` per project |

---

## Further Reading

- [Architecture](architecture.md) — full system overview, network topology, database schema
- [Design Decisions](decisions.md) — why mTLS over other auth approaches, why Docker hardening choices
- [Failure Model](failure-model.md) — how security-related failures (cert mismatch, agent unreachable) are handled
- [Multi-Server Setup](multi-server.md) — certificate architecture, mTLS setup, on-demand TLS
