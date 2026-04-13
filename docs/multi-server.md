# Multi-Server Setup

LiteBin supports running across multiple servers (nodes). One server acts as the **master** (orchestrator + dashboard + Caddy), and additional servers run as **agents** (worker nodes). Apps are deployed as Docker containers on any node.

## Prerequisites

- A master server set up via `curl -fsSL https://l8b.in | bash`
- One or more agent servers with Docker installed
- Root CA certificates generated during master setup (used for mTLS between master and agents)

## Adding an Agent

```bash
# On the agent server:
curl -fsSL https://l8b.in | bash -s agent
```

This installs the agent binary, generates mTLS certs (signed by the master's Root CA), and starts:
- **Agent app** — API server on port 8443 (mTLS, orchestrator-only)
- **Agent Caddy sidecar** — Reverse proxy on ports 80/443 (handles app traffic)

## Routing Modes

LiteBin has two routing modes, swappable from the dashboard. They differ in how user traffic reaches apps on remote agents.

### Master Proxy (default)

All traffic flows through the master server. Users hit the master Caddy, which proxies to the correct agent.

```
User → Master Caddy → (TLS, CA-verified) → Agent Caddy → Container
```

- DNS: Single wildcard `*.{domain}` A record pointing to master IP
- Master Caddy handles TLS termination for user connections
- Master Caddy connects to agent Caddy over TLS using mTLS PKI (agent.pem for server, ca.pem for verification)
- Agent Caddy routes to containers via Docker network (`litebin-{id}:{port}`)

**Trade-off: bandwidth** — the master sees 2x app traffic (downloads from agent, uploads to user). For high-traffic apps, consider Cloudflare DNS mode.

### Cloudflare DNS

DNS points directly to the agent server. Each agent handles its own traffic independently.

```
User → Cloudflare (per-project A record) → Agent IP → Agent Caddy → Container
```

- DNS: Per-project A records managed via Cloudflare API (automatic)
- Agent Caddy handles TLS via on-demand cert provisioning (Caddy auto-provisions certs via Let's Encrypt on first request, with permission check via `/internal/caddy-ask`)
- Master bandwidth: zero for app traffic
- Works even if master goes down (agent serves from persisted config)

**Trade-off: Cloudflare dependency** — requires a Cloudflare account with API access and zone management.

## How It Works Without Either Mode

If you're not using master proxy or Cloudflare DNS, you can still run agents by pointing DNS directly to agent IPs manually:

1. Create an A record for each project subdomain pointing to the agent's public IP
2. The agent Caddy sidecar listens on 80/443 and handles the subdomain
3. Caddy auto-provisions TLS via Let's Encrypt (on-demand TLS)

This is essentially what Cloudflare DNS mode automates — the manual version requires you to manage DNS records yourself.

Limitations of manual DNS without Cloudflare:
- No automatic DNS record management
- On-demand TLS requires a permission endpoint — the orchestrator pushes the agent's own `/internal/caddy-ask` endpoint by default (works without master), or the orchestrator's `/caddy/ask` endpoint if configured (requires master reachable from agent)
- Custom domains need manual DNS setup per domain

## Agent Independence

After initial registration and config push from the orchestrator, an agent can operate independently:

| Capability | Works without master? |
|---|---|
| Wake sleeping containers | Yes |
| Route traffic to running containers | Yes |
| Serve after agent restart | Yes (persisted config) |
| Serve after Docker/host restart | Yes |
| Issue new TLS certificates | Yes (certs are cached in Caddy's data volume; new/renewal uses agent's own `/internal/caddy-ask` permission endpoint) |
| Deploy new apps | No (needs orchestrator API) |
| Manage custom domains | No (needs dashboard) |

## Communication Paths

```
                    mTLS (port 8443)
Orchestrator ←------------------------→ Agent API
   (API, heartbeats,              (container ops,
    config push)                   wake reports)

                    HTTPS (port 443)
Master Caddy ←--------------------→ Agent Caddy
   (app traffic proxy,         (routes to containers
    TLS with CA verification)    via Docker network)

                    HTTP (port 8444, internal)
Agent Caddy ←------------------------→ Agent API
   (wake trigger for               (starts container,
    sleeping containers)            returns loading page)
```

- **Port 8443 (mTLS)**: Orchestrator ↔ Agent API only. Requires client certificate signed by the Root CA. Never handles user traffic.
- **Port 80/443 (HTTPS)**: User-facing traffic. In master_proxy mode, master Caddy proxies to agent Caddy here.
- **Port 8444 (HTTP, internal only)**: Agent Caddy ↔ Agent wake handler and on-demand TLS permission endpoint. Plain HTTP, no TLS — only reachable from the Docker network, not exposed on the host. Serves two purposes:
  1. Wake trigger for sleeping containers (cloudflare_dns mode)
  2. On-demand TLS permission check via `/internal/caddy-ask` — returns 200 for subdomains of the configured domain and known custom domain routes, 403 otherwise

## Request Flow (Master Proxy Mode)

Understanding the full request lifecycle is important for debugging. Here's what happens from deploy to serving traffic.

### Deploy Flow

```
Dashboard → Orchestrator API → mTLS (8443) → Agent API → Docker
                                                     ↓
                                              Container starts
                                                     ↓
                                              Agent rebuilds local Caddy
                                              (adds route: host → container)
                                                     ↓
                                              Orchestrator syncs master Caddy
                                              (adds route: host → agent_ip:443)
```

1. User deploys via dashboard/CLI → orchestrator sends `POST /containers/run` to agent over mTLS (port 8443)
2. Agent pulls image, creates and starts the container on `litebin-network`
3. Agent calls `rebuild_local_caddy()` — lists all running containers, builds Caddy JSON with routes + inline TLS cert (via `load_pem`), pushes to agent Caddy's admin API
4. Orchestrator syncs routes to master Caddy — adds route for `{project_id}.{domain}` → `{agent_ip}:443` with TLS transport (CA-verified)

### Normal Request (App Running)

```
User → DNS ({id}.{domain} → master IP)
     → Master Caddy (TLS termination)
     → matches route for {id}.{domain}
     → proxies to agent_ip:443 (TLS, server_name=agent, CA-verified)
     → Agent Caddy matches host header
     → proxies to litebin-{id}:{port} (Docker network)
     → Container responds
```

Key details:
- Master Caddy configures TLS transport with `root_ca_pem_files: ["/certs/ca.pem"]` and `server_name: "agent"` to match the agent cert's SAN (`DNS:agent`)
- Master Caddy preserves the original `Host` header (`{http.request.host}`) so the agent Caddy can match the correct project
- Agent Caddy routes to containers using Docker DNS names (`litebin-{project_id}:{internal_port}`), not `localhost` — this works because both the Caddy sidecar and project containers share `litebin-network`

### Sleeping App Wake Flow

```
User → Master Caddy → agent_ip:443 → Agent Caddy (no matching route)
                                        → returns 502
     ← Master Caddy catches 502 via handle_response
     → falls back to orchestrator waker
     → Orchestrator sends mTLS request to agent (start container)
     → Agent starts container, rebuilds local Caddy
     → Agent reports wake to orchestrator
     → Orchestrator syncs master Caddy routes
     → User refreshes → normal request flow
```

Key details:
- Agent Caddy's catch-all route returns a **static 502** (not a proxy) — this triggers master Caddy's `handle_response` for 502/503/504 status codes, which falls back to the orchestrator waker
- The waker shows a "Starting {project}..." page with a 1-second auto-refresh
- Once the container is running and routes are rebuilt, the next refresh reaches the app

## Certificate Architecture

```
Root CA (ca.pem) — generated once on master, trusted by all parties
├── Server cert (server.pem + server-key.pem) — master's mTLS client cert
├── Agent cert (agent.pem + agent-key.pem) — used two ways:
│   ├── Agent API mTLS server (loaded from /certs/ volume mount)
│   └── Agent Caddy TLS server (embedded inline via load_pem in Caddy JSON)
└── All certs are ECDSA P-256, 10-year validity, SAN=DNS:agent
```

How certs are used:

| Component | Cert Files | How Loaded |
|---|---|---|
| Agent API (Axum, port 8443) | `/certs/agent.pem` + `/certs/agent-key.pem` + `/certs/ca.pem` | File read at startup from volume mount |
| Agent Caddy (port 443) | Same agent.pem + agent-key.pem | Embedded inline in Caddy JSON via `load_pem` (pushed via admin API) |
| Master → Agent TLS | `/certs/ca.pem` (in master Caddy container) | Referenced in Caddy transport config via `root_ca_pem_files` |

The agent Caddy does **not** need certs mounted as files — the agent reads them from its own `/certs/` volume and embeds the PEM content directly in the Caddy JSON config. This avoids issues with cert file paths inside different containers.

### On-Demand TLS (Cloudflare DNS / Manual DNS modes)

In modes where users hit the agent Caddy directly, the agent cert alone isn't enough — the agent needs valid TLS certs for each project subdomain (e.g. `myapp.l8b.in`). Caddy provisions these automatically via on-demand TLS:

```
User → Agent Caddy (SNI=myapp.l8b.in)
     → No matching cert in load_pem (agent cert has SAN=agent, not *.l8b.in)
     → On-demand TLS triggers
     → Caddy calls permission endpoint: GET http://litebin-agent:8444/internal/caddy-ask?domain=myapp.l8b.in
     → Permission check: is myapp.l8b.in a subdomain of the configured domain? → Yes → 200 OK
     → Caddy provisions cert via ACME (Let's Encrypt, HTTP-01 challenge on port 80)
     → Cert cached, served for future requests
```

| Domain type | Permission check | Allowed? |
|---|---|---|
| `{project_id}.{domain}` | Subdomain of configured domain | Yes |
| Custom domain (e.g. `app.mycompany.com`) | Has a route in current Caddy config (pushed by orchestrator) | Yes |
| Arbitrary domain | Neither check passes | No |

In master_proxy mode, on-demand TLS is never triggered — all connections arrive with `SNI=agent` which matches the static cert.

## Container Startup Order (Agent)

The startup order matters for the agent to successfully push its config:

1. **Agent Caddy sidecar** starts first (via `run_agent_caddy`)
   - Loads initial Caddyfile: admin on `0.0.0.0:2019`, catch-all 502 on `:80`
   - No TLS yet — TLS is added when the agent pushes the full config

2. **Agent app** starts second (via `run_agent_container`)
   - Reads cert PEM files from `/certs/` volume
   - Connects to agent Caddy admin API at `litebin-agent-caddy:2019`
   - Pushes base config with TLS cert (`load_pem`) + catch-all 502 on `:80` and `:443`
   - If persisted config exists (from previous run), pushes that instead

Both containers must be on `litebin-network` for DNS resolution (`litebin-agent-caddy:2019`) to work. The install script calls `ensure_agent_network` before creating either container.

## Bandwidth Comparison

| Mode | Master bandwidth | Agent bandwidth | DNS requirement |
|---|---|---|---|
| Master Proxy | 2x (proxy in+out) | 1x (normal) | Wildcard A record |
| Cloudflare DNS | 0x | 1x (normal) | Per-project A record (auto) |
| Manual DNS | 0x | 1x (normal) | Per-project A record (manual) |

"1x" means the agent handles one copy of the request/response. "2x" means the master proxies both directions, seeing every byte twice.

## Public IP Resolution

Both master and agent auto-detect their public IP at startup by querying external services (ipify, AWS checkip, icanhazip). This IP is used for DNS records in `cloudflare_dns` mode and for agent reachability.

### Master

| Priority | Source | When |
|---|---|---|
| 1 | `PUBLIC_IP` env var | Set manually (e.g. behind NAT) |
| 2 | Auto-detection | Startup, if env var is empty |

The detected value is stored in the `nodes` table as the "local" node. Update via Dashboard > Settings or set `PUBLIC_IP` in `.env` and restart.

### Agent

| Priority | Source | When |
|---|---|---|
| 1 | Dashboard value (DB) | Set manually via Nodes page — **never overwritten** |
| 2 | `AGENT_PUBLIC_IP` env var | Set in agent's `.env` (e.g. behind NAT) |
| 3 | Auto-detection | Agent startup, if env var is empty |

The agent reports its IP in every `/health` response. The orchestrator only writes it to the DB if the current value is empty — a dashboard-set value always takes priority. If the agent is behind NAT, set its IP via the dashboard or `AGENT_PUBLIC_IP` in the agent's `.env`.

## Setup Checklist

### Master Proxy Mode
1. Set up master server
2. Add agents via dashboard or `bash -s agent`
3. Set `ROUTING_MODE=master_proxy` (default)
4. Ensure wildcard DNS `*.{domain}` → master IP
5. Ensure `PUBLIC_IP` is set correctly (auto-detected at startup, or set manually if behind NAT)

### Cloudflare DNS Mode
1. Set up master server
2. Add agents via dashboard or `bash -s agent`
3. Set `ROUTING_MODE=cloudflare_dns`
4. Set `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ZONE_ID` in master `.env`
5. Ensure `PUBLIC_IP` is set correctly (auto-detected at startup, or set manually in `.env` if behind NAT)
6. Create DNS-only A records for `{DASHBOARD_SUBDOMAIN}.{domain}` and `{POKE_SUBDOMAIN}.{domain}` pointing to master IP
7. Do NOT create a wildcard (`*`) record — app subdomains are managed automatically via the Cloudflare API
8. Ensure all agents are reachable from the internet on port 80/443

### Manual DNS Mode
1. Set up master server
2. Add agents via dashboard or `bash -s agent`
3. Use either routing mode (master_proxy is simpler)
4. For direct-to-agent: create A records manually for each project subdomain
5. Ensure agent ports 80/443 are open on the firewall

## Future: Built-in DNS Server

Both non-proxy modes (Cloudflare DNS and manual DNS) avoid the 2x bandwidth cost of master proxy, but each has a dependency — either on Cloudflare or on manual DNS management. A built-in DNS server on the master would eliminate both:

```
User → Recursive DNS → Master DNS (authoritative for {domain})
                         → Returns agent IP for {project_id}.{domain}
                         → Returns master IP for dashboard/admin subdomains

User → Agent Caddy → Container (direct, no master in data path)
```

### Why

- **No Cloudflare dependency** — the master owns the zone and answers DNS queries directly
- **No bandwidth cost** — users connect directly to agent IPs, master only handles small DNS queries
- **Automatic record management** — same as Cloudflare DNS mode, but using the master's own DNS instead of an external API
- **Full self-hosting** — no external service required beyond the VPS fleet

### Requirements

1. **Authoritative DNS server** running on the master (e.g. embedded Hickory DNS, or a lightweight container like CoreDNS)
2. **NS records** at the domain registrar pointing `{domain}` NS to the master's IP
3. **Zone management** — orchestrator creates/updates A records when projects are deployed, stopped, or moved between agents
4. **Wildcard SOA/NS** — master DNS is authoritative for `{domain}`, returns the correct agent IP per subdomain

### How It Would Work

| Event | DNS Action |
|---|---|
| Project deployed to agent | Create A record `{project_id}.{domain}` → agent public IP |
| Project stopped/sleeping | Remove A record (or point to master IP for wake page) |
| Project moved to another agent | Update A record to new agent IP |
| Agent goes offline | Remove all A records for that agent's projects |
| Dashboard/admin subdomain | A record points to master IP (unchanged) |

### Zone File Example

```
{domain}            IN  SOA  ns1.{domain} admin.{domain} 2026041201 3600 600 604800 300
{domain}            IN  NS   ns1.{domain}
ns1.{domain}        IN  A    {master_ip}
dashboard.{domain}  IN  A    {master_ip}
poke.{domain}       IN  A    {master_ip}
myapp.{domain}      IN  A    {agent_1_ip}
otherapp.{domain}   IN  A    {agent_2_ip}
```

### Migration from Cloudflare DNS Mode

Most of the infrastructure already exists:

| Piece | Status |
|---|---|
| Per-project A record management | Already implemented (Cloudflare API client) |
| Route sync triggers (deploy, stop, move) | Already implemented |
| Agent IP tracking in database | Already implemented (`nodes.public_ip`) |
| Zone management logic | Needs DNS provider abstraction (swap Cloudflare → internal) |

The main new work is:
- Run an authoritative DNS server alongside the orchestrator (container or embedded)
- Abstract the DNS provider behind a trait so `cloudflare_dns` and `internal_dns` are swappable
- Update install script to set up NS records and open port 53

This would make LiteBin fully self-hosted with zero external dependencies for DNS, while keeping the direct-to-agent traffic flow that avoids the bandwidth cost of master proxy mode.
