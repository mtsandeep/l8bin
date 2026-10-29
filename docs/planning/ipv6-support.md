> **Status: NOT STARTED — independent track** · code audit verified 2026-10-28

# IPv6 / IPv6-only Support

First-class IPv6 support across three tiers: (1) correct behavior on any host where Docker IPv6 is enabled, (2) installable on IPv6-only VPSes (the cheapest tier: Vultr $2.50, Hetzner v6-only, free tiers), (3) IPv6 nodes + AAAA routing in multi-server. No competing self-hosting PaaS (Coolify, Dokploy, CapRover) supports IPv6-only today — the ecosystem answer is "add an IPv4" ([representative thread](https://community.n8n.io/t/self-hosted-n8n-on-coolify-in-a-hetzner-vps-cant-connect-to-google-services/83245/9)).

---

## Case study that drove this (2026-10-28, Vultr t1)

IPv6-only VPS, full install required five manual host fixes:

| Layer | Symptom | Manual fix applied |
|---|---|---|
| Host DNS | GitHub/Docker Hub unreachable (no AAAA, host resolver has no DNS64) | `/etc/resolv.conf` → nat64.net public DNS64 + `chattr +i` |
| Docker daemon | `apt-get` fails in image builds (default bridge is IPv4-only) | `daemon.json` `{ipv6, fixed-cidr-v6}` |
| Compose network | Caddy can't reach Let's Encrypt, orchestrator can't reach caddy admin | `enable_ipv6: true` on `litebin-network` |
| Installer IP detection | Tryout domain built from the **NAT64 gateway's** IPv4 (`159-69-108-193.sslip.io`) | `set-domain` + `.env` to dashed-IPv6 sslip host |
| Dashboard proxy | 502s — nginx binds `0.0.0.0:80`, orchestrator dials AAAA-first with no fallback | patched `nginx.conf` `listen [::]:80;` (local only) |

Final state works; the nginx patch and compose/network/daemon tweaks are all clobbered by the next installer update (see §5). t1 remains as the test rig for this track.

Key runtime discovery: on IPv6-enabled Docker networks, names resolve **AAAA-first**; our Rust clients dial the AAAA and do **not** fall back to A, while most listeners bind IPv4-only. That mismatch — not anything IPv6-specific — is the root defect class for M1.

---

## Code audit findings

Zero `"AAAA"`, `Ipv6Addr`, `ipv6`, or `daemon.json` matches anywhere in the Rust codebase or installer. Everything IPv4-shaped:

### 1. HTTP clients — no address-family fallback anywhere

No reqwest client in orchestrator/agent/common sets `happy_eyeballs`, `dns_resolver`, or any resolver option (the only custom resolver in the repo is the CLI's `LocalhostAware`, [cli/src/http.rs:50](../../cli/src/http.rs) — precedent to copy).

Victims on an IPv6-enabled network (dial by container name → AAAA-first → target is v4-only):

| Client | Dials | Breaks |
|---|---|---|
| `proxy_client` (orchestrator [main.rs:359](../../orchestrator/src/main.rs), `reqwest::Client::new()`, zero config) | `dashboard:80`, `litebin-{id}:{port}`, readiness probes | dashboard proxy (observed 502 on t1), every project waker/probe dial |
| `proxy_client` (agent [main.rs:102](../../agent/src/main.rs)) | `litebin-{id}:{port}` | agent-side wake + readiness probes |
| `CaddyClient` ([litebin-common/src/caddy.rs:21](../../litebin-common/src/caddy.rs)) | `localhost:2019` / `caddy:2019` | route sync push (observed on t1) — admin binds `0.0.0.0:2019` |
| per-node `build_node_client` ([nodes/client.rs:49](../../orchestrator/src/nodes/client.rs)) | `{node.host}:{agent_port}` | all agent control traffic on IPv6-first DNS |
| `build_remote_proxy_client` ([client.rs:72](../../orchestrator/src/nodes/client.rs)) | `resolve_node_addr` [client.rs:96-102](../../orchestrator/src/nodes/client.rs) takes **first** `lookup_host` answer only and pins it via `.resolve("agent", addr)` | silently pins the wrong family for the master→agent mTLS proxy |
| `wake_wait.rs` readiness probes ([litebin-common/src/wake_wait.rs:133](../../litebin-common/src/wake_wait.rs)) | `http://{container:port}/` | deploys/wakes never go ready (v6 name, v4-only app) |

### 2. Binds — IPv4-only listeners

| Site | Bind |
|---|---|
| Orchestrator HTTP — `HOST` default `"0.0.0.0"` ([config.rs:36](../../orchestrator/src/config.rs), seeded by installer `.env`) | v4-only |
| Agent ×3 — wake `8444`, upload `8445`, mTLS `agent_port` — all `SocketAddr::from(([0,0,0,0], p))` ([agent/main.rs:164,196,234](../../agent/src/main.rs)) | v4-only |
| Caddy admin `"0.0.0.0:2019"` — [routing.rs:378](../../litebin-common/src/routing.rs), [caddy_config.rs:196,396](../../orchestrator/src/cloudflare_router/caddy_config.rs), [agent waker/caddy.rs:409,443](../../agent/src/routes/waker/caddy.rs), bootstrap `Caddyfile:2`, agent Caddyfile [install.sh:1371](../../install.sh) | v4-only |
| Dashboard nginx `listen 80;` ([dashboard/nginx.conf:2](../../dashboard/nginx.conf)) | v4-only (t1's observed 502) |
| Raw-port publishes `host_ip: Some("0.0.0.0")` ([run.rs:108-114](../../litebin-common/src/docker/container/run.rs)) | v4-only host side |
| Public-port / managed-proxy loopback publish `127.0.0.1` ([run.rs:14-21,74-77](../../litebin-common/src/docker/container/run.rs)) | v4 loopback |

(Caddy app listeners `:80`/`:443` and the haproxy `*:2375` are already dual-stack.)

### 3. Public-IP detection — hard IPv4 filter

- [litebin-common/src/net.rs:35](../../litebin-common/src/net.rs): `parse::<Ipv4Addr>()` gate — IPv6 responses discarded; providers are 3-of-4 explicitly v4. On an IPv6-only host `public_ip` stays empty → DNS sync skipped, direct upload disabled, agent-caddy ask endpoint falls back.
- Installer mirror: [install.sh:848-854](../../install.sh) (same providers, no validation) → sslip transform `tr '.' '-'` [install.sh:876-879](../../install.sh) produces an invalid hostname for an IPv6 string, and on NAT64 hosts captures the gateway's IPv4 (t1 bug).
- Escape hatches that exist today: `PUBLIC_IP` / `AGENT_PUBLIC_IP` env overrides; `dns_target` setting; manual `set-domain`.

### 4. Address-as-string formatting — no IPv6 bracket handling

`format!("{}:{}", host, port)` and `https://{ip}` sites that produce invalid/broken strings for IPv6 literals:

- `agent_base_url` [client.rs:185-191](../../orchestrator/src/nodes/client.rs) — all AgentClient URLs
- remote upstream dials `{}:443` [routing_helpers.rs:137,267](../../orchestrator/src/routing_helpers.rs) → Caddy `dial` strings
- direct-upload `base_url` [images.rs:287-294](../../orchestrator/src/routes/images.rs) (+ CLI round-trip [upload.rs:72](../../cli/src/upload.rs))
- agent-caddy ask endpoint from `public_ip` [caddy_config.rs:386-391](../../orchestrator/src/cloudflare_router/caddy_config.rs)
- orchestrator bind concat [main.rs:505-507](../../orchestrator/src/main.rs)
- Host-header port-stripping `host.split(':').next()` — [waker/handler.rs:799,852](../../orchestrator/src/routes/waker/handler.rs), [agent wake.rs:354,711](../../agent/src/routes/waker/wake.rs), [heartbeat.rs:55](../../litebin-common/src/heartbeat.rs), [wake_wait.rs:114](../../litebin-common/src/wake_wait.rs) — mangles `[v6]:port`
- registry host parsing [registry.rs:96-103](../../litebin-common/src/registry.rs)
- `localhost`-special-cases without `::1`: [handler.rs:803](../../orchestrator/src/routes/waker/handler.rs), [cli ship/ui.rs:54](../../cli/src/ship/ui.rs); dev-mode wake/heartbeat URLs use `http://localhost:{port}` against v4-bound orchestrator ([connect.rs:159-176](../../orchestrator/src/routes/nodes/connect.rs))
- github webhook reachability scan is IPv4-literal-only ([github/manifest.rs:38-67](../../orchestrator/src/github/manifest.rs)) — an IPv6 sslip-style host is misclassified as reachable

### 5. DNS layer — A-only, unproxied

- Record type `"A"` hard-coded at [cloudflare_router/dns.rs:142,176](../../orchestrator/src/cloudflare_router/dns.rs) and [domain_job.rs:90](../../orchestrator/src/routes/global_settings/domain_job.rs); `upsert_record(..., ttl=1, proxied=false)` always grey-cloud.
- `dns_records` ledger has no record-type column ([dns_ledger.rs:13-16](../../orchestrator/src/dns_ledger.rs)); existing-record detection is type-filtered to A, so a pre-existing AAAA for a desired name is invisible.
- Record content = `config.public_ip` / `nodes.public_ip` — strings that can only be IPv4 today.
- Platform-domain preflight ([domain_job.rs:103-131](../../orchestrator/src/routes/global_settings/domain_job.rs)): resolves via `lookup_host` (family-agnostic) but string-compares against the IPv4-only expected value.
- Orange-cloud proxied records work in front of Caddy's on-demand TLS (verified on t1's master-proxy path), so `proxied` support is a config/plumbing question, not a design risk.

### 6. Docker integration — IPv6 never configured, v4 field assumed

- Network creation passes `..Default::default()` — no `EnableIPv6`, no IPAM ([docker/image.rs:38-54,292-308](../../litebin-common/src/docker/image.rs)); compose template networks likewise ([install.sh:673-676](../../install.sh)); `ensure_agent_network` raw `docker network create` ([install.sh:427-431](../../install.sh)).
- `wait_for_network_ready` polls only the inspect **IPv4** `ip_address` field ([container/wait.rs:74-99](../../litebin-common/src/docker/container/wait.rs)) — v6-only-attached containers never appear ready.
- DOCKER_HOST injections `tcp://127.0.0.1:{port}` ([compose_run.rs:256-263](../../litebin-common/src/compose_run.rs) + 3 agent/orchestrator mirrors); `is_port_ready` dials `127.0.0.1` ([docker/mod.rs:291-294](../../litebin-common/src/docker/mod.rs)).
- Intra-node addressing is otherwise sound: deterministic container names + Docker DNS aliases, never container IPs — nothing to change in the addressing model itself.

### 7. Installer — IPv4-biased output, update flow clobbers manual IPv6 fixes

- No `daemon.json` handling anywhere; no `enable_ipv6` emission; DNS hints print **A** records only ([install.sh:1016-1032](../../install.sh)).
- Tryout sslip path assumes IPv4 (§3); non-root/firewall hints are family-neutral (ports only).
- No post-install HTTP health check (only `docker inspect`), so family failures surface late.
- Update flow: compose regen **overwrites** manual edits after a diff prompt ([install.sh:1576-1603](../../install.sh)); Caddyfile regenerated **silently** ([install.sh:502-514](../../install.sh)); `dashboard/nginx.conf` deleted and re-extracted every update ([install.sh:709-730](../../install.sh)). `.env` survives. Windows installer mirrors the Caddyfile clobber.
- Agent containers recreated from hard-coded `docker run` flags — manual network tweaks lost.

---

## Design decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Client family-fallback strategy | One shared client-builder helper in `litebin-common` with a custom `dns_resolver` (extend the CLI's `LocalhostAware` precedent) that orders/merges A+AAAA so a refused family falls through to the other. Verify hyper-util happy-eyeballs behavior empirically first — t1 proved stock reqwest does **not** fall back. Route every client in §1 through it. |
| D2 | Bind strategy | Dual-stack everywhere: `HOST` default → `[::]` (Linux dual-stack accepts v4), agent's three `SocketAddrV4` → `SocketAddr::new(IpAddr::from(Ipv6Addr::UNSPECIFIED), p)`, Caddy admin → `[::]:2019` in all six config sites + bootstrap Caddyfiles, nginx `listen [::]:80;`. |
| D3 | Address formatting | Small helpers in `litebin-common`: `format_host_port(host, port)` (brackets when host parses as IPv6) + `split_host_port`/Host-header parser (bracket-aware). Replace every site in §4. Envelope: accept and canonicalize IPv6-literal `nodes.host`. |
| D4 | Public-IP detection | `detect_public_ip()` returns `IpAddr`: add `api64.ipify.org` / `ipv6.icanhazip.com`, keep v4 preference on dual-stack (A-record ecosystem), accept v6 when it's all there is. Installer mirrors; sslip transform handles both forms (dashed v6). |
| D5 | DNS record family | Type follows the node's `public_ip` family: A for v4, AAAA for v6; both records when both known. Add `record_type` to the ledger + list/upsert paths. `proxied` becomes a routing setting (default off; required on for IPv6-only nodes to serve IPv4 visitors via Cloudflare). Preflight compares family-correctly. |
| D6 | Docker IPv6 networks | Conditional: only emit `enable_ipv6` / `EnableIPv6` when the host is IPv6-only (or user opts in). Hard precondition: M1a landed — enabling v6 today breaks every waker/probe dial to v4-only containers. IPv6-only hosts must get it; dual-stack hosts should not silently change behavior. |
| D7 | Update-flow preservation | Compose regen preserves an existing `enable_ipv6` key; nginx `[::]` fix ships upstream so the local patch dies; Caddyfile regen diff-prompts like compose instead of silent overwrite (general fix, IPv6 is just the first casualty). |
| D8 | Readiness gating | `wait_for_network_ready` accepts either `ip_address` or `GlobalIPv6Address`; loopback dials (`127.0.0.1` → `localhost` or `[::1]` alternative) go through the D3 helper. |

---

## Milestones

Claim ladder: **M1a** → "IPv6-only hosts supported (with documented host prep)" · **M2+M3** → "IPv6-only hosts fully supported" (zero manual steps; residual limits are inherent to the box, see test plan).

### M1a — Minimal: IPv6-only works with documented host prep

The smallest code set that makes the five host-prep steps below sufficient — without it, no host prep is enough (proven on t1: the 502s were code behavior, not host config). Roughly two PRs.

Code:

1. D1 shared client builder + resolver, wired into the four critical clients: orchestrator `proxy_client`, agent `proxy_client`, `CaddyClient` (admin dials), node clients — including the `resolve_node_addr` first-answer-only bug.
2. D2 essential binds, one-liners each: nginx `listen [::]:80;`, orchestrator `HOST` default, agent ×3 listeners, Caddy admin `[::]:2019` (all six generated-config sites + bootstrap Caddyfiles).
3. D8 readiness gating — `wait_for_network_ready` accepts `GlobalIPv6Address`.

Host prep the FAQ entry documents (user-side, permanent characteristics of an IPv4-less box):

1. `resolv.conf` → DNS64 resolvers + `chattr +i` — outbound egress to IPv4-only endpoints (GitHub, Docker Hub, registries)
2. `/etc/docker/daemon.json` `{ipv6, fixed-cidr-v6}` + daemon restart — image builds
3. `enable_ipv6: true` on `litebin-network` + `docker compose up -d` — container egress; **re-add after every litebin update** until M2's D7
4. Domain entered manually at install (real domain, or dashed-IPv6 sslip host) — skips the tryout auto-detect NAT64-IP trap
5. Cloudflare AAAA + orange, routing mode `master_proxy` — how IPv4 visitors reach the box

**Done when:** a t1-class box, following only the FAQ host prep, installs and passes the feature checklist (deploy → wake → sleep → serve on dashed-v6 sslip and on a CF-proxied domain). No regression on IPv4-only (CI + local stack).

### M1b — Remaining sweep (dual-stack parity, no host-prep dependency)

1. D3 bracket helpers + replace every formatting/Host-parsing site in §4 (IPv6-literal `nodes.host`, Host headers, registry hosts, upload URLs).
2. Remaining §1 clients through the D1 builder: registries ad-hoc client, capabilities self-test, agent activity/wake-report reporters.
3. `::1` localhost special-cases; dev-mode `http://localhost:{port}` report URLs against the now-`[::]`-bound orchestrator.
4. Raw-port publishes dual-stack ([run.rs](../../litebin-common/src/docker/container/run.rs) `host_ip: Some("0.0.0.0")` sites).
5. Loopback-family cleanup for DOCKER_HOST injections and `is_port_ready`.
6. GitHub manifest IPv6-sslip reachability scan (else defer to M4).

**Done when:** the same dual-stack host with `enable_ipv6` hand-applied passes the checklist including IPv6-literal `nodes.host` and raw ports; nothing in §1/§4 remains unconverted.

### M2 — IPv6-only master install

1. D4 detection (installer + runtime + sslip dashed-v6 transform).
2. D6 conditional daemon.json/compose/agent-network IPv6 emission + D7 preservation.
3. Installer DNS hints: AAAA when v6-only; NAT64/DNS64 hint when GitHub unreachable and host is v6-only.
4. FAQ "IPv6-only VPS" entry extended with installer-automation notes (the entry itself ships with M1a).

**Done when:** the full t1 sequence is a zero-manual-step `install.sh` run on a fresh IPv6-only VPS.

### M3 — IPv6 nodes & Cloudflare AAAA

1. D5 record-family support end-to-end (ledger type, list/upsert, cleanup, token proof).
2. `proxied` routing setting (orange-cloud mode) + docs (Full-strict SSL mode note).
3. Preflight family-aware comparison.
4. Multi-server docs: IPv6-only agents, `nodes.host` as v6 literal (bracket-safe through D3).

**Done when:** an IPv6-only agent registers to an IPv6-only master over public DNS; project domains get AAAA records; proxied records serve IPv4 visitors.

### M4 — optional polish

- IPv6-literal `nodes.host` everywhere (upload URLs, remote dials) verified e2e.
- 464XLAT/`clatd` note in docs for literal-IPv4 edge cases on IPv6-only hosts.
- GitHub webhook reachability scan learns IPv6 sslip hosts.

---

## Test plan

- **t1 (Vultr IPv6-only)** — permanent rig; M1a acceptance target (FAQ host prep only), then M2 e2e: reinstall from scratch, sslip tryout domain auto-correct, deploy → wake → sleep → wake, custom domain with Cloudflare AAAA + orange.
- **Dual-stack rig** — any existing dev VPS + hand-applied `enable_ipv6` for M1a/M1b (this is also the config real users hit when they enable IPv6 themselves — the primary regression surface).
- **IPv4-only** — local stack + CI: zero behavior change guard.
- Per-milestone checklists: deploy single-service + compose apps whose containers bind `0.0.0.0`, `:80`-style wildcard binds, and `[::]` binds; wake-from-sleep; transfers/export-import; registries pull; direct upload; GitHub connect webhook.

## Docs to touch

- New FAQ entry (M2), `multi-server.md` AAAA/IPv6-agent section (M3), `configuration.md` `PUBLIC_IP`/`dns_target` family notes, installer hint strings, this doc's status line per milestone.
