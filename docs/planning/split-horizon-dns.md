> **Status: SHIPPED (2026-11-19) — user-facing docs live in [topics/domains.md](../topics/domains.md); this file keeps the engineering record. Remaining: VPS verification (runbook below), then default `split_dns_enabled` to on. M3 (self-hosted authoritative DNS) scoped below.**

# Same-Host Reachability: NAT Hairpin Fix (Hosts Injection + Split-Horizon DNS)

Deployed app containers cannot reach same-host services through their public URLs — the reported case is an app calling the master API at `https://dash.{domain}`, which resolves to the host's own public IP; Docker drops bridge-sourced packets to published ports (NAT hairpin), so the request times out while the same URL works fine from the host or the internet. Verified live on vps-b558ec04 (2026-11-18). The same class breaks app→app (`{id}.{domain}`), custom domains of same-host projects, and same-node app→app on agents in `cloudflare_dns` mode. litebin.dev ships an app-side retry-via-gateway fallback; the fix belongs in l8bin so every deployed app gets it for free.

No competing PaaS solves this: Coolify's answer is a shared network + "use container names, not domains" — their equivalent issue ([discussion #2887](https://github.com/coollabsio/coolify/discussions/2887)) is unanswered. Our plan keeps public URLs working unmodified, without giving up per-project network isolation.

Two-phase fix plus one scoped follow-up:

1. **M1 — hosts-file injection** at the single container-creation funnel. Fixes app→master (the reported case). Zero new components.
2. **M2 — split-horizon resolver sidecar** (`litebin-dns`, hickory-dns subcommand carried by both orchestrator and agent binaries): fixes app→app and custom-domain hairpin everywhere, on the master **and every agent, enabled by default**. Master: no opt-out (core infrastructure). Agents: opt-out env (in `master_proxy` mode their zone is empty/forward-only — that is what the opt-out is for). ~8–15 MB RSS per host, accepted. Zones are mode-aware (see D8): wildcards on the master in `master_proxy`, per-node exact names in `cloudflare_dns` (a name in the wrong node's zone would black-hole at a caddy that doesn't route it). Per-node zones are pushed over the existing mTLS channel and **persisted locally** — agent resolution never depends on the master being up.
3. **M3 — self-hosted authoritative DNS** (scoping only): direct-to-node routing without the Cloudflare dependency. Previously dropped because it needed our own DNS server; M2's sidecar is that server. `desired_dns_records()` in [cloudflare_router/dns.rs](../../orchestrator/src/cloudflare_router/dns.rs) is already provider-agnostic; `RoutingProvider::sync_dns_only` is the seam for a `SelfHostedDnsRouter`.

Rejected alternatives: attaching projects to `litebin-network` (isolation loss, doesn't help public-URL configs), enumerating all project domains into every `/etc/hosts` (staleness trap), dnsmasq as a separate image (image drift), resolver inside the orchestrator process (crash/update coupling).

---

## Code audit findings

- **Single creation funnel**: every project container CREATE goes through `DockerManager::run_service_container` ([litebin-common/src/docker/container/run.rs:61](../../litebin-common/src/docker/container/run.rs)) — master + agent, all deploy paths, demo clones, batch-run, import, crash-recreates. CRIU checkpoint/restore operates on the existing container, so create-time config persists across sleep/wake cycles.
- **START fast paths keep baked config**: `start_existing_container` ([litebin-common/src/docker/container/lifecycle.rs:78](../../litebin-common/src/docker/container/lifecycle.rs)) — used by wakers and restarts when nothing changed. Injection therefore adopts on the next CREATE (redeploy, recreate, env change, crash), not on plain wake. Both states are valid; no mass-recreate migration.
- `RunServiceConfig.extra_hosts` exists but is dead ([litebin-common/src/types.rs:514](../../litebin-common/src/types.rs)) — never consumed; the single-service path builds its `HostConfig` fresh. Compose-declared `extra_hosts` do arrive via `bollard_host_config` ([compose-bollard/src/mapping.rs:61](../../compose-bollard/src/mapping.rs)).
- `HostConfig.dns` is used nowhere; compose `dns:` is declared ignored in the compat table ([compose-bollard/src/compat/tables.rs:68](../../compose-bollard/src/compat/tables.rs)).
- **Platform hostnames**: `platform_hosts()` ([orchestrator/src/platform.rs:101](../../orchestrator/src/platform.rs)) = apex + dashboard + poke × current/previous/staged domains. Platform loads before `DockerManager::new` (orchestrator main.rs:201 vs :250). Domain changes mutate state at runtime → aliases must be refreshable.
- **Gateway IP**: `litebin-network` is a custom bridge whose gateway is NOT 172.17.0.1 (default bridge). Verified locally: `bridge` → 172.17.0.1, `litebin-network` → 172.18.0.1. The default-bridge gateway is a host-local IP reachable from every container network — that is the bind/publish target. Installer templates the real value from `docker network inspect bridge`.
- **Zone source**: `desired_dns_records()` ([cloudflare_router/dns.rs:10](../../orchestrator/src/cloudflare_router/dns.rs)) computes the full name→IP set (project subdomains, dashboard, poke, custom domains + www, aliases, demo hosts). Platform records are always grey-cloud (`proxied: false`, dns.rs:258).
- **Push precedents**: orchestrator → caddy `/load`, orchestrator → agent mTLS `AgentClient` ([orchestrator/src/nodes/client.rs](../../orchestrator/src/nodes/client.rs)), agent-internal unpublished listeners (agent :8444). `sync_caddy` ([orchestrator/src/routes/manage/helpers.rs:283](../../orchestrator/src/routes/manage/helpers.rs)) is the choke point after every route/domain change.
- **Reserved ports**: `litebin_reserved_host_ports()` ([litebin-common/src/types.rs:110](../../litebin-common/src/types.rs)) — consumed only by the raw-ports filter (run.rs:113). 53 must join the list.
- **Installer**: compose generated at install.sh:641 (three services; a `dns` service slots after caddy), env heredoc :976, `update_master` regenerates + diffs compose (:1622) so a new service flows through the existing update prompt. No port preflight exists today. Binaries ship as release artifacts — a subcommand needs zero release-pipeline changes.
- No DNS crates anywhere in the workspace; tokio `full` (UDP) already available in both binaries.

## Accepted limitations (found live)

- **R1 verified on vps-b558ec04 (2026-11-20) — with a ufw condition**: container → host-IP → published 443 is dropped when ufw is active (Docker excludes same-bridge traffic from the port DNAT, so the packet lands in host INPUT). Fix: ufw allows the RFC 1918 ranges (10/8, 172.16/12, 192.168/16) to host ports 80/443 — covering Docker's default pools (including the 192.168 spillover after ~15 networks) and any custom address pool — baked into `configure_ufw` in install.sh (all roles). With the rule in place, an app container resolved and reached `https://dash.litebin.dev/health` end-to-end.

- **Docker Desktop dev boxes cannot host the sidecar** (confirmed 2026-11-19): Desktop binds published ports through the host-OS proxy, where the bridge IP `172.17.0.1` doesn't exist (it lives inside the VM) — the publish fails to bind and the toggle shows the bind error. Split DNS is effectively Linux-host-only; M1 hosts entries work fine on Desktop. The VPS runbook below is the real verification.
- **Self-image sidecars must skip the registry pull** (found live): local-only image names 404 on pull — `ensure_sidecar_container` now pulls only when the image is absent locally (also saves rate limits on drift-recreates).
- `docker compose down` takes the sidecar down with the stack (same window as caddy); the `litebin-sidecars` project only protects it from orphan removal.

## Design decisions

> **M2c pivot (2026-11-19, supersedes the compose/.env approach below where they conflict):**
> the sidecar is **orchestrator-managed** via the backup-sidecar pattern
> (`ensure_sidecar_container` — extended with port publishing, per-sidecar caps,
> create-time env, and drift-recreate covering image id, cmd, env, ports,
> labels, and memory) and **toggled from Settings → General** (`split_dns_enabled`
> DB setting + token/bind-IP generated and stored at first enable; bind IP
> detected via the Docker API — no installer involvement). All sidecars run
> under the **`litebin-sidecars` compose project** (constant
> `SIDECAR_COMPOSE_PROJECT`), so `docker compose down`/`--remove-orphans` in the
> install dir never removes them; the existing `litebin-backup` migrates there
> on its next ensure. Agents: the master's first zone push bootstraps the
> agent's own sidecar (relay ensures it, token persisted in the agent's data
> dir); a monitor activates per-container injection only while the sidecar
> exists and is healthy. The compose service, installer `.env` keys, preflight,
> and `run_agent_dns` from M2a/M2b were all removed. Dashboard status is real
> (healthy / starting / error from the reconcile loop), not just the toggle
> value. Residual accepted behaviors: `compose down` takes the sidecar down
> with the stack anyway (same window as caddy), and recreate-on-drift replaces
> the container (zone data persists in its named volume).

| # | Decision |
|---|---|
| D1 | `DockerManager` gains interior-mutable `host_aliases: Arc<RwLock<Vec<String>>>`, `dns_servers: Arc<RwLock<Option<Vec<String>>>>`, `split_dns_healthy: AtomicBool` + setters. `::new` signature unchanged; defaults = no injection. Used by both binaries. |
| D2 | Aliases: agent = `host.docker.internal` only. Orchestrator = `host.docker.internal` + `platform_hosts()`. Kill-switch `L8B_DISABLE_HOST_ALIASES=1`. Refreshed after domain changes. |
| D3 | `merge_extra_hosts(existing, lb_entries)` in run.rs: user entries first and win; ours appended, dedup'd by hostname (case-insensitive); skipped for host-network services; applied to both paths; wires the dead `RunServiceConfig.extra_hosts` field. |
| D4 | `dns:` injected only when `dns_servers` set AND `split_dns_healthy` — `dns_options: ["timeout:1","attempts:1"]` bounds sidecar-restart stalls to ~1 s. List = `[DNS_BIND_IP, upstreams…]`. |
| D5 | Env: `L8B_DISABLE_HOST_ALIASES`, `DNS_BIND_IP` (installer-templated, default 172.17.0.1), `DNS_PUSH_TOKEN` (openssl rand), `UPSTREAM_DNS`, `L8B_DISABLE_SPLIT_DNS=1` (agent-only opt-out; master ignores). Rollout staged dark, then default-on after VPS verification. |
| D6 | Migration: lazy adoption via CREATE-time injection. Scan-imported containers adopt on next recreate (documented gap). |
| D7 | Sidecar = compose service (not `SidecarSpec` — the bridge-IP publish must show in update diffs): same image as the node's binary, `command: dns`, `"${DNS_BIND_IP:-172.17.0.1}:53:53"` + `/udp`, `restart: unless-stopped`, `dns-data` volume, 64 m cap / 12 m reservation. Repo compose under `profiles: ["dns"]`; installer emits it only when enabled. |
| D8 | Resolver (hickory, in litebin-common so both binaries carry the `dns` subcommand): UDP+TCP :53; `ZoneSpec {answer_ip, ttl 5, wildcard_domains, exact_names}`. Mode-aware zones: `master_proxy` → master = wildcard `*.{domain}` + previous-domain grace + apex + custom domains, agents = empty (forward-only); `cloudflare_dns` → per-node exact names only (locally-hosted projects' names + custom domains + alias hosts; dashboard/poke/apex only in the master's zone; cross-node names never). Wildcard match = equal or ends-with `.entry`. **AAAA on zone names → empty NOERROR** (forwarding would hairpin via the public AAAA on IPv6-enabled installs). **CDN/WAF rule**: a custom domain enters the zone only if its public resolution points at this host's IP (hairpin candidates — mirrors the existing "LiteBin-owned IPs" concept, dns.rs:155); CDN-fronted domains stay external automatically and nothing is silently bypassed; the janitor re-checks resolution so a domain later moved behind a CDN drops out. No user-facing "force external path" option — CDN-fronted names already stay external; hairpin names never had a working external path. (Future escape hatch if ever asked: per-project skip of `dns:` injection.) |
| D9 | Zone push: sidecar HTTP :5380 (unpublished, host's litebin-network only, bearer token; empty token = listener off); `GET /health`, `POST /load` full-snapshot (204 on same hash). Master pushes locally; per-node agent zones ride the existing mTLS AgentClient via a new `POST /internal/dns-zone` agent endpoint that relays to its local sidecar. Zone persists in `dns-data` → agent resolution survives master outages. Push sites: boot, `sync_caddy` piggyback, custom-domain change, domain-job persist, janitor reconcile. |
| D10 | Degradation: never inject a dead resolver (health monitor gates CREATE-time injection); sidecar down → M1 hosts entries still work; upstream fallback in the per-container list. Rootless docker → auto-disable split-DNS on that host. |
| D11 | Reserved port 53 added (a user `0.0.0.0:53` raw-port publish would shadow the bridge bind). Installer: `ensure_env_keys` for existing `.env`; port-53 preflight (systemd-resolved's 127.0.0.53 does not conflict with 172.17.0.1 — stated in the message); no ufw change for the internal resolver (bridge IP is not public; M3 would open public 53). |
| D12 | Not doing: compose `dns:` reconciliation (compat-table wording update only). |

Memory budget: M1 zero; +~2 MB binary (hickory) per binary; ~8–15 MB RSS per enabled sidecar host; ~60 bytes resolv.conf per container. Note in docs/dev/memory-footprint.md when M2a lands.

## Milestones

### M1 — hosts injection (fixes the reported case)
`merge_extra_hosts` + D1/D2/D3 wiring in litebin-common; orchestrator alias wiring (`host_alias_list()` in platform.rs, main.rs, domain-job refresh); agent alias wiring; unit tests; changelog. Verify: local stack deploy → `docker exec cat /etc/hosts`; **R1 falsification on the VPS before M2a starts** (append `172.17.0.1 dash.{domain}` to a deployed app's /etc/hosts and curl the dashboard URL — if that fails, the gateway→published-port mechanism is broken on that host and M2 must be re-planned before building).

### M2a — dns subcommand, dark launch
Hickory deps (lockfile same commit); `litebin-common/src/dns_server/{mod,zone,http,forward}.rs`; `dns` dispatch arm in both binaries (own tracing init — keeps the cli.rs one-shot convention true); installer .env keys, conditional `dns` service in master + agent compose paths, caps, `ensure_env_keys`, port-53 preflight; repo compose `dns` profile services. Zone/HTTP/A-record/AAAA unit tests. Nothing binds :53 on a default install.

### M2b — enable path
Orchestrator enable block (token validation, `set_dns_servers`, health monitor, initial push); mode-aware zone builders (reuse `desired_dns_records()` + node-hosting filter + CDN exclusion); agent `/internal/dns-zone` relay + local enable/health/persistence; reserved port 53; compat wording; docs; changelog. VPS verification: resolv.conf, in-app dashboard curl (the bug), dig gateway vs external, custom-domain push, sidecar restart, agent same-node app→app (cloudflare_dns, after an agent-host R1 check), master-down agent resolution, raw-port-53 refusal.

### M3 — self-hosted authoritative DNS (scoping only)
`SelfHostedDnsRouter` sibling to `CloudflareDnsRouter` implementing `RoutingProvider`, reusing the M2 sidecar as the authoritative server and `desired_dns_records()` as the record source via `sync_dns_only`; installer NS-delegation prompts (glue records, AXFR ACL); agents as zone secondaries (hickory AXFR); `ufw allow 53`. Unblocked by M2 — the old plan's "needs our own DNS server" blocker is gone.

## Test plan

- Unit: `merge_extra_hosts` semantics (user-wins, dedup, case-insensitive, None+empty→None); `host_alias_list` covers previous/staged; zone matching (wildcard hit/miss, suffix-not-substring `evil{l8bin.com}`, per-node zones exclude cross-node names); `ZoneSpec` serde round-trip; `/load` 200/204/401/400; A synthesis + AAAA empty NOERROR; `desired_internal_zone` (wildcards, enumerated customs + www, background excluded, CDN-proxied excluded, grey-cloud included); health-gate (unhealthy → no `host.dns`).
- Live-`#[ignore]`: forwarder against a real resolver.
- Local (Docker Desktop): `--profile dns up`, dig via litebin-network, M1 `/etc/hosts` checks.
- VPS (user-run): the M2b sequence above.

## VPS verification runbook (gates the default-on flip)

On vps-b558ec04, in order:

```bash
# 0. R1 falsification — BEFORE enabling anything. If this fails, the
#    gateway→published-port mechanism is broken on this host: stop, file the
#    iptables findings, re-plan (contingency: secondary litebin-network attach).
docker exec <app-container> sh -c \
  "echo '172.17.0.1 dash.litebin.dev' >> /etc/hosts && wget -qO- --no-check-certificate -T 5 https://dash.litebin.dev/ && echo R1-OK"

# 1. After upgrading to the release (M1 active):
#    redeploy any app from the dashboard, then:
docker exec <app-container> grep host-gateway /etc/hosts        # → host.docker.internal + dash.<domain> lines
docker exec <app-container> wget -qO- --no-check-certificate -T 5 https://dash.litebin.dev/health && echo M1-OK

# 2. Enable split DNS (M2) — Dashboard → Settings → General → Routing & DNS →
#    flip the "Same-host reachability (split DNS)" toggle. Watch the status
#    line turn healthy (it shows the bind IP), then:
docker ps --filter name=litebin-dns                        # orchestrator-managed sidecar running
# 3. Redeploy an app, then:
docker exec <app-container> cat /etc/resolv.conf                # nameserver 172.17.0.1 + fallback, options timeout:1 attempts:1
docker exec <app-container> nslookup dash.litebin.dev 172.17.0.1   # → 172.17.0.1
docker exec <app-container> nslookup github.com 172.17.0.1         # → real answer (forwarding)
docker exec <app-container> wget -qO- --no-check-certificate -T 5 https://dash.litebin.dev/ && echo M2-OK
dig @1.1.1.1 dash.litebin.dev +short                           # external DNS unchanged (public IP)

# 4. Resilience + guards:
docker restart litebin-dns && docker exec <app-container> nslookup github.com 172.17.0.1  # still resolves (fallback)
docker inspect litebin-dns --format '{{index .Config.Labels "com.docker.compose.project"}}'  # litebin-sidecars
docker logs litebin-dns --tail 5                                # /load 200s, health
# raw port 53 deploy attempt → refused with reserved-port warning

# 5. Agent node (cloudflare_dns installs): same R1 check against the agent's
#    own public name first, then confirm agent resolv.conf + same-node
#    app→app via public URL, zone persistence across litebin-agent-dns
#    restart, and resolution surviving a master outage.
```

## Docs to touch

[faq.md](../topics/faq.md) / [domains.md](../topics/domains.md) — "apps reaching same-host services from inside a deployment"; [multi-server.md](../topics/multi-server.md) — agent-side behavior; [configuration.md](../topics/configuration.md) — env table; [llms.txt](../llms.txt) — topic bullet; [memory-footprint.md](../dev/memory-footprint.md) — sidecar note; [compat tables](../../compose-bollard/src/compat/tables.rs) — `dns:` wording; CHANGELOG per milestone.
