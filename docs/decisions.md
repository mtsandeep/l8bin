# Design Decisions

Why LiteBin is built the way it is.

---

## Why Rust

The orchestrator, agent, and CLI are all written in Rust.

- **Single binary deployment** — no runtime to install, no version conflicts, no GC pauses. One file per component.
- **Low memory footprint** — the orchestrator uses ~15-20 MB RAM idle. On a $5 VPS with 1 GB RAM, that leaves room for several app containers.
- **Reliable long-running services** — Rust's ownership model eliminates data races and use-after-free bugs at compile time. For a process that manages other processes and handles network traffic 24/7, this matters.
- **Fast cold starts** — scale-to-zero only works if waking is fast. Rust's startup time is measured in milliseconds, not seconds.
- **Async first-class** — tokio + axum handles concurrent wake requests, heartbeats, and container operations without thread overhead.

Alternatives considered: Go (good fit, but larger binaries and less memory-efficient for idle services), Node.js (too much memory for a background orchestrator, single-threaded), Python (too slow for container management loops).

## Why Caddy

Caddy serves as the reverse proxy on both master and agent nodes.

- **Dynamic config via admin API** — LiteBin pushes JSON config programmatically. No config file reloads, no SIGHUP, no race conditions. Add a route, remove a route, change an upstream — all via HTTP POST to `localhost:2019`.
- **Automatic HTTPS** — Let's Encrypt certificates are provisioned on demand. No certbot, no cron jobs, no manual renewal. Caddy handles it.
- **JSON config** — The admin API accepts and returns JSON, which maps directly to Rust structs. No template rendering, no config file parsing.
- **On-demand TLS** — In Cloudflare DNS mode, agents need certs for arbitrary subdomains. Caddy's on-demand TLS with a permission endpoint (`/caddy/ask`) solves this cleanly — provision a cert only if the domain belongs to a known project.
- **Lightweight** — ~20 MB idle. Comparable to nginx, with none of the config complexity.

Alternatives considered: nginx (no dynamic API — requires config file generation and reload), Traefik (heavier, more complex, designed for larger orchestration), HAProxy (no automatic HTTPS).

## Why SQLite

All persistent state (users, projects, nodes, settings) is stored in a single SQLite file.

- **Zero ops** — No database server to install, configure, backup, or update. The file lives at `data/litebin.db` and that's it.
- **WAL mode** — Write-Ahead Logging allows concurrent reads while a write is in progress. The orchestrator can serve API requests while the janitor writes status updates.
- **Fast enough** — LiteBin manages tens to hundreds of projects, not millions of rows. SQLite handles this without breaking a sweat. Benchmarks show it outperforms Postgres for reads under ~1000 QPS on a single connection.
- **Single-file backup** — Copy the `.db` file and you have a complete backup. No dump/restore, no pg_dump, no point-in-time recovery complexity.
- **No network overhead** — The database is local to the orchestrator. No TCP connection, no authentication, no connection pooling. Every query is a local file read.

Alternatives considered: Postgres (unnecessary complexity for this scale), MySQL (same), embedded KV stores like sled/redb (SQL is more ergonomic for the query patterns here — joins, aggregations, filtering).

## Why Docker (not Kubernetes)

LiteBin manages containers directly via the Docker API using the `bollard` crate.

- **Right-sized complexity** — LiteBin manages 1-50 apps on 1-5 servers. Kubernetes is designed for 1000+ containers across dozens of nodes with rolling deployments, autoscaling, and service mesh. That's 100x more complexity than needed.
- **Docker is universal** — Every developer knows Docker. Every CI/CD pipeline can build a Docker image. Every registry (Docker Hub, GHCR, ECR) serves Docker images. No vendor lock-in.
- **No agent daemon** — Docker is already running on the server. No additional system services, no kubelet, no containerd shim.
- **Simple networking** — One bridge network, DNS-based service discovery. No CNI plugins, no overlay networks, no network policies.
- **Container lifecycle is simple** — `docker run`, `docker stop`, `docker rm`. The orchestrator handles scheduling. No pods, no ReplicaSets, no StatefulSets.

Kubernetes would add: etcd, kube-apiserver, kube-scheduler, kube-controller-manager, kubelet, containerd, CNI — six additional system services, each consuming memory and requiring maintenance. For LiteBin's use case, this is pure overhead.

## Why Scale-to-Zero

Apps are stopped after 15 minutes of inactivity (configurable) and woken on the next request.

- **Side projects are idle 99% of the time** — A portfolio site, a demo app, a weekend hackathon project. They get traffic when you share the link, then nothing for days or weeks.
- **Resource efficiency** — A sleeping container uses 0 CPU and 0 memory (it's stopped, not paused). On a $5 VPS with 1 GB RAM, this means you can register 20 projects but only pay for the 2-3 that are actively serving traffic.
- **Fast wake** — Starting a stopped container takes 1-3 seconds. The user sees a loading page with auto-refresh. By the time they notice, the app is running.
- **No user action needed** — The janitor handles stopping. The waker handles starting. The user just visits the URL.

Scale-to-zero is the core feature that makes running many apps on a small VPS practical. Without it, you'd need to manually stop idle apps or pay for a larger server.

## Why Two Routing Modes

### Master Proxy (default)

All user traffic flows through the master server's Caddy instance.

- **Simple DNS setup** — One wildcard `*.{domain}` A record. Done.
- **Centralized TLS** — Master Caddy handles all certificate provisioning. Agents don't need public-facing TLS.
- **Familiar pattern** — Works like a traditional reverse proxy setup.

Trade-off: master sees 2x bandwidth (downloads from agent, uploads to user). Fine for low-traffic side projects.

### Cloudflare DNS

DNS points directly to agent nodes. Each agent runs its own Caddy and handles TLS independently.

- **No master bottleneck** — App traffic goes directly to agents. Master bandwidth is zero for user traffic.
- **Agent independence** — After one config push from the orchestrator, agents can wake containers, route traffic, and serve requests even if the master goes down entirely.
- **Automatic DNS** — Cloudflare API creates/deletes A records per project. No manual DNS management.

Trade-off: requires a Cloudflare account with API token and zone management.

The two modes are hot-swappable from the dashboard. Start with master proxy for simplicity, switch to Cloudflare DNS when you need the resilience or bandwidth savings.

## Why mTLS for Master-Agent Communication

All orchestrator-to-agent traffic uses mutual TLS with self-signed certificates.

- **No application-level auth needed on the agent** — The TLS handshake is the auth. Connections without a valid client cert are rejected before any HTTP request is processed.
- **No shared secrets over the network** — Certificates are generated once on the master and distributed during agent setup. No API keys in env vars, no token rotation.
- **Works over public internet** — Agents can be on different VPS providers, different networks, even home servers behind NAT. mTLS encrypts and authenticates everything.
- **Simple to reason about** — Either the connection has a valid cert signed by the Root CA, or it doesn't. No OAuth flows, no JWT validation, no session management.

The trade-off is that adding a new agent requires distributing a cert bundle. This is intentional — it limits who can connect to your agents to people you've explicitly given certs to.
