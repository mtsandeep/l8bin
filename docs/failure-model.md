# Failure Model

How LiteBin handles failures at every layer. What breaks, what degrades, and what keeps working.

---

## Component Failures

### Master (Orchestrator) Goes Down

| Capability | Impact | Recovery |
|---|---|---|
| Deploy new apps | Broken — no API | Restart orchestrator |
| Dashboard access | Broken — no API | Restart orchestrator |
| Agent heartbeat | Agents detect master is unreachable, continue autonomously | Master comes back, heartbeats resume |
| Wake sleeping apps (Mode A) | Broken — waker runs on orchestrator | Restart orchestrator |
| Wake sleeping apps (Mode B) | **Works** — agent handles wake locally | Automatic |
| Serve running apps (Mode A) | **Works** — Caddy on master continues proxying | Automatic |
| Serve running apps (Mode B) | **Works** — agent Caddy handles traffic independently | Automatic |
| Auto-stop idle apps | Broken — janitor runs on orchestrator | Restart orchestrator |
| Activity tracking (Mode A) | Broken — log tailer runs on orchestrator | Restart orchestrator |
| Activity tracking (Mode B) | Agents continue tailing logs but can't report to master | Master comes back, agents flush queued hosts |
| TLS certificate provisioning | New certs won't be issued (on-demand TLS needs `/caddy/ask`) | Restart orchestrator |
| Cloudflare DNS management | Broken — no API calls | Restart orchestrator |

**Key takeaway:** In Mode B, a master outage is nearly invisible to users. Agents serve running apps, wake sleeping apps, and route traffic independently. The only things that break are deploy and management operations.

### Agent Goes Down

| Capability | Impact | Recovery |
|---|---|---|
| Apps on that agent | Unreachable | Agent restarts, containers restart, Caddy rebuilds |
| Heartbeat | Orchestrator marks node `offline` after 3 missed heartbeats (~90s) | Agent restarts, heartbeat resumes, status → `online` |
| Master Caddy routes | Routes to agent IP return 502 → orchestrator waker shows error page | Agent comes back, next request succeeds |
| Cloudflare DNS records | Records still point to agent IP (DNS not removed on agent failure) | Manual cleanup or agent reconnect |
| Other agents | Unaffected | N/A |
| Local node apps | Unaffected | N/A |

### Caddy Goes Down (Master)

| Capability | Impact | Recovery |
|---|---|---|
| All app traffic (Mode A) | Broken — no TLS termination, no routing | Docker restarts Caddy (restart policy: `unless-stopped`) |
| All app traffic (Mode B) | **Unaffected** — agents handle their own traffic | N/A |
| Dashboard | Broken — no reverse proxy | Docker restarts Caddy |
| Orchestrator | Unaffected — runs independently | N/A |

### Caddy Goes Down (Agent)

| Capability | Impact | Recovery |
|---|---|---|
| App traffic to that agent (Mode A) | Master Caddy gets 502 → shows error page | Docker restarts agent Caddy |
| App traffic to that agent (Mode B) | Broken — no TLS, no routing | Docker restarts agent Caddy |
| Agent wake handler | Broken — catch-all won't work without Caddy | Docker restarts agent Caddy |
| Agent API | Unaffected — separate process on port 5083 | N/A |

### Database Corruption

| Capability | Impact | Recovery |
|---|---|---|
| All operations | Broken — SQLite returns errors | Restore from backup (`data/litebin.db`) |

SQLite WAL mode is resilient to crashes — uncommitted transactions are rolled back automatically. The most common "corruption" is a full disk, not actual data loss.

**Prevention:** SQLite's WAL mode + `PRAGMA journal_mode=WAL` + `PRAGMA synchronous=NORMAL` provides crash safety with good performance. The orchestrator runs `PRAGMA busy_timeout=5000` to handle concurrent access gracefully.

---

## Network Failures

### Master Can't Reach Agent (mTLS)

| Symptom | Cause | Fix |
|---|---|---|
| `agent unreachable` in logs | Firewall blocking port 5083 | Open port 5083 on agent (OS firewall + cloud firewall) |
| Connection timeout | Agent IP changed (NAT, DHCP) | Update agent IP in dashboard or set `AGENT_PUBLIC_IP` |
| TLS handshake failure | Cert mismatch (regenerated certs, old agent certs) | Run `bash -s agent --update-certs` |

### Agent Can't Reach Master

| Symptom | Cause | Fix |
|---|---|---|
| Wake-report fails | Master down or unreachable | Fire-and-forget — agent continues serving traffic, retries on next wake |
| Heartbeat fails | Master down or DNS resolution failure | Agent continues autonomously, reconnects when master is back |

### DNS Misconfiguration

| Symptom | Cause | Fix |
|---|---|---|
| Wildcard not resolving | Missing `*.{domain}` A record | Create wildcard DNS record (Mode A) |
| Subdomain not resolving | Cloudflare API token expired or zone mismatch | Check `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ZONE_ID` |
| TLS cert failure | Domain doesn't match cert | Caddy will provision new cert via Let's Encrypt |

---

## Wake Failure Paths

### Background Wake Fails (Both Modes)

```
Background wake fails (Docker error, timeout, image gone)
  │
  ├─ Lock stays in project_locks (NOT removed)
  ├─ Auto-cleanup spawned: remove after 60s
  │
  Next browser refresh → completed=true, success=false
  ├─ Error page: "Failed to start {project}. Retrying in 30 seconds..."
  │
  After 60s → lock auto-removed → next refresh tries fresh wake
```

| Failure | Recovery |
|---|---|
| Container image doesn't exist | Error page, 30s retry, then 60s lock expiry, then fresh attempt |
| Container fails to start | Same — error page, retry cycle |
| Docker daemon unresponsive | Wake times out (60s), error page, retry cycle |
| Port conflict on restart | Orchestrator auto-assigns new port (`host_port=0`) |

### Master Down During Agent Wake (Mode B)

```
Agent wakes container, rebuilds local Caddy
  │
  ├─ report_wake_to_master() → connection refused
  │   └─ Logged as debug, ignored (fire-and-forget)
  │
  ├─ Container is running, Caddy route exists locally
  └─ App serves traffic normally

When master comes back:
  ├─ Next heartbeat → master reconciles DB state
  └─ Or next wake → wake-report succeeds
```

### Docker Daemon Restarts (Port Drift)

When Docker restarts, ephemeral port bindings (`host_port=0`) may change. This is handled differently per mode:

| Mode | Handling |
|---|---|
| Mode A (local) | Orchestrator waker inspects actual port via Docker API, updates DB, resyncs Caddy |
| Mode A (remote) | `docker start` preserves original port binding. If stale, falls back to `docker recreate` with new auto-assigned port |
| Mode B | Agent waker uses `docker start` (preserves port). If container gone, `rebuild_local_caddy()` discovers running containers from Docker API with correct ports |

---

## Concurrency Edge Cases

### Concurrent Requests During Wake

Single-flight dedup via `project_locks` (Rust `DashMap`). All concurrent visitors get the instant loading page. Only one background wake task runs per project.

### Concurrent Deploys to Same Project

Per-project mutex (`project_locks`). Second deploy waits for the first to complete. No race conditions on container creation or route updates.

### Janitor vs Wake Race

| Scenario | Result |
|---|---|
| Janitor stops container while wake is in progress | Wake's single-flight lock prevents janitor from touching the project (running status is set before container starts) |
| Wake starts container while janitor is evaluating | Janitor queries `status = 'running'` — if wake already set it, janitor skips. If not yet, janitor may stop the container, but wake's `docker start` will fail and fall back to `docker recreate` |
| Janitor and wake both try to update `last_active_at` | SQLite serializes writes. No conflict — both are `UPDATE` statements with the same effect |

---

## Degradation vs Failure

Some conditions don't cause hard failures but degrade gracefully:

| Condition | Degradation | User sees |
|---|---|---|
| Agent unreachable (heartbeat) | Orchestrator marks node `offline`, image stats timeout | Dashboard shows node as offline, image stats page may be slow |
| Cloudflare API rate limit | DNS record creation delayed | App deployed but DNS may take longer to propagate |
| Disk full | SQLite writes fail, container creation fails | Error pages, deploy failures |
| High memory on VPS | OOM killer may stop containers or the orchestrator | Apps become unreachable, dashboard errors |
| Container exceeds memory limit | Docker kills the container (OOM) | 502 error page, auto-wake on next request |

See [Troubleshooting FAQ](faq.md) for debugging steps for specific failure symptoms.
