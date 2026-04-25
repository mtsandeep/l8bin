# Security

## Threat Model

LiteBin runs untrusted user code (Docker containers) on shared infrastructure. The primary threats:

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

- Master holds a server cert signed by the Root CA
- Each agent holds a client cert signed by the same Root CA
- Both sides verify the other's certificate chain
- Certs are ECDSA P-256 (production installer), valid for 10 years

```
Root CA (self-signed, ECDSA P-256)
├── Master server cert (SAN: hostname + IP)
└── Node client cert (CN: <node-name>, one per agent)
```

---

## Container Hardening

| Control | Value |
|---|---|
| `cap_drop` | `ALL` |
| `cap_add` | `CHOWN`, `DAC_OVERRIDE`, `SETGID`, `SETUID`, `NET_BIND_SERVICE`, `KILL` |
| `security_opt` | `no-new-privileges` |
| `pids_limit` | 4096 |
| `log_config` | max-size 10m, max-file 3 |
| `memory` | 256 MiB default, per-project override |
| `nano_cpus` | 0.5 cores default, per-project override |
| `network_mode` | `litebin-network` (or per-project `litebin-{project_id}` for multi-service) |
| `restart_policy` | `no` (orchestrator manages lifecycle) |

### Capability strategy

All 14 default Linux capabilities are dropped, then 6 are added back for app compatibility. `no-new-privileges` ensures these cannot be escalated further.

| Capability | Reason |
|---|---|
| `CHOWN` | Frameworks and build tools need to change file ownership |
| `DAC_OVERRIDE` | Apps need to read/write files they don't own |
| `SETGID` | Required by many runtimes and package managers |
| `SETUID` | Required by many frameworks for user switching |
| `NET_BIND_SERVICE` | Some apps bind to privileged ports |
| `KILL` | Process management within the container |

Capabilities still blocked (not added back): `CAP_NET_RAW` (no packet sniffing), `CAP_SYS_ADMIN`, `CAP_SYS_PTRACE`, and others.

---

## Agent Security

Agent API has no application-level auth — security relies entirely on mTLS:

- mTLS is mandatory (agent fails to start without certs)
- Each agent has a unique client cert signed by the private Root CA
- Connections without a valid cert are rejected at the TLS handshake
- Agent port (5083) is open to all IPs — mTLS is the auth layer, not firewall rules
- Config is pushed from orchestrator over mTLS via `POST /internal/register`, no secrets in agent env vars

---

## Docker Socket

Both orchestrator and agent mount `/var/run/docker.sock`. This is required for container management. The risk is mitigated by:

- App containers are on isolated networks with no Docker socket access
- Agent requires mTLS for all connections
- Orchestrator requires session or deploy token auth
- App containers have restricted capabilities

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
