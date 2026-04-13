# Frequently Asked Questions & Troubleshooting

## Table of Contents

- [mTLS Certificates](#mtls-certificates)
- [Agent Not Connecting](#agent-not-connecting)
- [Dashboard is Slow / Endpoints Timing Out](#dashboard-is-slow--endpoints-timing-out)
- [Docker Logs](#docker-logs)
- [Networking & Firewalls](#networking--firewalls)

---

## mTLS Certificates

### How to check if certs were created properly on the master

```bash
# Check host filesystem (run as root)
ls -la /etc/litebin/certs/
# Expected files: ca.pem, server.pem, server-key.pem, agent.pem, agent-key.pem

# Check inside the orchestrator container
docker exec litebin-orchestrator ls -la /certs/
```

### How to check if certs were deployed properly on the agent

```bash
# Check host filesystem
ls -la /etc/litebin/certs/
# Expected files: ca.pem, agent.pem, agent-key.pem

# Check inside the agent container
docker exec litebin-agent ls -la /certs/

# Test the agent health endpoint with mTLS
curl -v --cert /etc/litebin/certs/agent.pem \
  --key /etc/litebin/certs/agent-key.pem \
  --cacert /etc/litebin/certs/ca.pem \
  https://localhost:5083/health
# Expected: JSON response with version, memory_total, cpu_cores, etc.
```

### Cert files and their roles

| File | Used By | Purpose |
|------|---------|---------|
| `ca.pem` | Master + Agent | Root CA that signs both master and agent certs |
| `server.pem` | Master (orchestrator) | Master's TLS certificate (client cert when talking to agents) |
| `server-key.pem` | Master (orchestrator) | Master's private key |
| `agent.pem` | Agent | Agent's TLS certificate (server cert for incoming connections) |
| `agent-key.pem` | Agent | Agent's private key |

### How to show the cert bundle for adding a new agent

```bash
# On the master
curl -fsSL https://l8b.in | bash -s certs --show-bundle
```

### How to regenerate all certs (invalidates all agents)

```bash
# On the master — all agents will need to re-run --update-certs after this
curl -fsSL https://l8b.in | bash -s certs --regenerate
```

### How to update certs on an existing agent

```bash
# On the agent — paste the bundle from `certs --show-bundle` when prompted
curl -fsSL https://l8b.in | bash -s agent --update-certs
```

---

## Agent Not Connecting

### Symptoms

- Agent stuck in `pending_setup` status in the dashboard
- Orchestrator logs show repeated `agent unreachable` warnings
- Node status never changes to `online`

### Step-by-step debug

**1. Is the agent process actually running?**

```bash
# On the agent server
docker ps --filter "name=litebin-agent" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
```

If status is not `Up`, check the logs (see [Docker Logs](#docker-logs) section).

**2. Is the agent listening on the expected port?**

```bash
# On the agent server
ss -tlnp | grep 5083
```

**3. Is the master reachable from the agent?**

```bash
# On the agent server — replace MASTER_IP with your master's public IP
nc -zv MASTER_IP 443 -w 5
```

**4. Is the agent reachable from the master?**

```bash
# On the master server — replace AGENT_IP with your agent's public IP
nc -zv AGENT_IP 5083 -w 5
```

If this times out, see [Networking & Firewalls](#networking--firewalls).

**5. Check orchestrator heartbeat logs**

```bash
docker logs litebin-orchestrator --tail 50 2>&1 | grep -i "heartbeat\|agent unreachable\|pending_setup"
```

---

## Dashboard is Slow / Endpoints Timing Out

### Symptoms

- `/nodes/image-stats` takes a long time to respond
- Node list or other pages load slowly

### Common cause

The orchestrator calls agents sequentially with a 30-minute timeout. If any agent is unreachable, each request blocks for ~30 seconds before timing out.

### Debug

```bash
# Check orchestrator logs for failed agent calls
docker logs litebin-orchestrator --tail 100 2>&1 | grep -i "failed\|timeout\|unreachable"
```

### Fix

Ensure all registered agents are reachable. Remove or decommission unreachable nodes from the dashboard. See [Agent Not Connecting](#agent-not-connecting) to fix the unreachable agent.

---

## Docker Logs

### Container names

| Container | Service |
|-----------|---------|
| `litebin-orchestrator` | Master API server, heartbeat, janitor, project management |
| `litebin-dashboard` | Frontend web UI (Nginx) |
| `litebin-caddy` | Reverse proxy on the master (TLS termination, routing) |
| `litebin-agent` | Worker node daemon (runs on remote servers) |
| `litebin-agent-caddy` | Reverse proxy on the agent (app traffic) |

### Useful log commands

```bash
# Last 100 lines of orchestrator logs
docker logs litebin-orchestrator --tail 100

# Last 100 lines of agent logs
docker logs litebin-agent --tail 100

# Follow orchestrator logs live (Ctrl+C to stop)
docker logs litebin-orchestrator -f

# Filter for heartbeat/connection issues
docker logs litebin-orchestrator --tail 200 2>&1 | grep -i "heartbeat\|connect\|unreachable"

# Filter for agent errors
docker logs litebin-agent --tail 200 2>&1 | grep -i "error\|warn"

# Filter for Caddy routing issues
docker logs litebin-caddy --tail 100 2>&1 | grep -i "error\|502\|503"

# Filter for image-related issues
docker logs litebin-orchestrator --tail 100 2>&1 | grep -i "image"
```

### What orchestrator logs tell you

| Log message | Meaning |
|-------------|---------|
| `heartbeat: node online` | Agent health check succeeded, node is healthy |
| `heartbeat: agent unreachable` | Cannot connect to agent (network/firewall issue) |
| `heartbeat: node marked offline` | 3 consecutive heartbeat failures, node is now offline |
| `heartbeat: attempting to connect pending_setup node` | Trying to push config to a new agent |
| `heartbeat: config pushed to agent` | Agent registration succeeded |
| `failed to get image stats from remote node` | Agent unreachable when fetching image stats |
| `no client for remote node` | Agent was never registered (missing mTLS client in pool) |

---

## Networking & Firewalls

### Required ports

| Port | Protocol | Where | Purpose |
|------|----------|-------|---------|
| 80 | TCP | Master + Agent | HTTP (redirects to HTTPS) |
| 443 | TCP + UDP | Master + Agent | HTTPS (app traffic, QUIC) |
| 5083 | TCP | Agent only | Orchestrator-to-agent mTLS API |

Port 443 on the agent is only needed if using `cloudflare_dns` routing mode. With `master_proxy` (default), only 5083 is needed externally.

### Checking UFW (OS-level firewall)

```bash
sudo ufw status
sudo ufw allow 5083/tcp
```

### Cloud provider firewalls

UFW only controls the OS-level firewall. Most cloud providers (DigitalOcean, Vultr, AWS, GCP, Hetzner) have **separate cloud firewalls** that block traffic before it reaches the VM. You must open port 5083 in both places:

- **DigitalOcean:** Networking → Firewalls → Inbound Rules → Add Custom TCP 5083
- **Vultr:** Settings → Firewall → Edit Firewall Group → Inbound Rules → Add TCP 5083
- **Hetzner:** Firewalls → Add rule → TCP 5083
- **AWS:** Security Groups → Inbound Rules → Custom TCP 5083

### Quick connectivity test

```bash
# From master to agent (replace AGENT_IP)
nc -zv AGENT_IP 5083 -w 5
# Success: "Connection to AGENT_IP 5083 port [tcp/*] succeeded!"
# Failure: "timed out" or "Connection refused"
```
