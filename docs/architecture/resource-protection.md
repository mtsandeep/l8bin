# Resource Protection

How the LiteBin stack stays alive — and stays responsive — when hosted apps saturate CPU or exhaust RAM. The goal: on a loaded node, the platform is always accessible, so you can watch what's happening, manage apps, and kill the offender from the dashboard.

The design uses **floors and victim ordering, not ceilings** — stack services are never memory-capped. They may burst freely (deploy buffering, traffic spikes); protection works by deciding *who gets evicted* and *who gets killed*, never by restricting the stack itself.

---

## The guarantee

On every node (master and agent), under any app-driven CPU or memory pressure:

1. **Apps die first** — the OOM killer ranks every app container above the entire LiteBin stack.
2. **A CPU slice always remains** — the scheduler guarantees the stack its share even when apps peg every core.
3. **The stack stays resident** — under memory pressure the kernel evicts and swaps app pages first; the dashboard doesn't die of swap thrash.

## Settings

On the master, the installer writes a minimal `docker-compose.yml` plus a separate `docker-compose.override.yml` carrying these settings — compose auto-merges the override on every invocation, so the main file stays clean and the protections are still creation-time config. Agent services use the equivalent `docker run` flags. The split follows one rule — whoever creates a container sets its protections: the installer creates the stack, LiteBin's code creates apps and sidecars (`run.rs`, `sidecar.rs`).

| Service | `oom_score_adj` | `cpu_shares` | `mem_reservation` |
|---|---|---|---|
| orchestrator | -300 | 26214 | 28m |
| dashboard | -300 | 26214 | 8m |
| caddy | -300 | 26214 | 30m |
| agent | -300 | 26214 | 28m |
| agent-caddy | -300 | 26214 | 30m |

Backup sidecars (`sidecar.rs`) are the one LiteBin-owned exception: they get a **hard cap** of 256 MB + 0.5 CPU, so a runaway backup job dies at its own limit instead of sinking the host.

### What each knob does

**`oom_score_adj: -300` — victim ordering.** The kernel's OOM killer picks victims by score ≈ RSS adjusted by this value. The host landscape is: dockerd runs at **-500**, journald at **-250**, everything else (apps, sshd, containerd-shims) at **0**. The stack at -300 nests between them:

```
killed first   apps (0) → shims, sshd (0) → journald (-250) → LiteBin stack (-300) → dockerd (-500)   killed last
```

Dockerd outranks the stack deliberately: if the daemon dies, containers keep running (shims parent them) but the platform loses all manageability — no deploys, no wake, no dashboard actions. The stack only becomes a kill candidate after everything above it is already dead.

**`cpu_shares: 26214` — a CPU share under saturation.** Docker converts shares to cgroup v2 `cpu.weight` (26214 → 1000); app containers, which set no shares, run at the default weight 100. Two things this is *not*: it is **not a cap** — when CPU is free, weights restrict nobody, and a single app may use the whole core; and it is not an absolute reservation — the guarantee is proportional. Whenever stack and apps are runnable at the same instant, a stack container is entitled to:

```
1000 / (1000 + 100 × N)    of the contended CPU, N = number of saturated ("pegged") apps
```

The stack's actual demand is 1–3%, so it is scheduled essentially instantly under any realistic load: against one pegged app its entitlement is 91%, against 50 apps ~17%, and it only falls to 5% at ~190 pegged apps per core. CPU is compressible — saturation can slow the stack's *work*, but never stalls or kills it.

**`mem_reservation` — a reclaim floor (`memory.low`).** Pages of a service that is *within* its floor are evicted last: under host-wide pressure, app pages get reclaimed and swapped first while the stack's working set stays resident. The semantics are all-or-nothing at the boundary: a service that grows *above* its floor (orchestrator mid-deploy, Caddy under heavy traffic) simply loses protection and becomes a normal memory citizen — never restricted, never capped. When the burst passes, the floor re-engages on its own.

Floors are sized to measured resident footprints ([memory footprint](../dev/memory-footprint.md)): orchestrator 17–30 MB resident, dashboard 2–5 MB, Caddy 23–39 MB heap-capped.

### Why no hard caps on the stack

A first deploy spikes the orchestrator's RSS to ~94 MB on multi-core hosts and traffic-heavy Caddy legitimately floats to ~60 MB+ ([measurements](../dev/memory-footprint.md)). A ceiling would convert those normal bursts into self-inflicted OOM kills. Floors plus victim ordering achieve survival without restricting the stack. Apps keep their existing hard caps — default 256 MB / 0.5 CPU, per-app overrides in settings, `restart: no` so an OOM'd app stays dead until the next request wakes it — but a cap above host capacity (2 CPU on a 1-core node, 8 GB on a 1 GB host) is a no-op, so aggregate overload is always arbitrated by the stack-side mechanisms on this page.

---

## Scenario matrix

| Scenario | Apps | Stack |
|---|---|---|
| Stack bursts (deploy → ~100 MB orchestrator) | unaffected | grows freely; reclaim floor pauses while above it, re-engages after |
| One app pegs CPU | slowed at most (own cap + weights) | guaranteed CPU slice; dashboard responsive |
| One app outgrows its 256 MB cap (swap present) | grows → swaps → cgroup OOM kill → 502 → auto-wake | untouched — container-local event |
| Many apps exhaust RAM | die fattest-first, permanently (`restart: no`) | resident + usable throughout; dies 3rd-from-last |
| Same, no swap | die sooner, less thrash | same ordering |
| Leak with zero apps running | n/a | survives shims/sshd/journald; only dockerd outranks it |
| Stack component itself leaks | may starve (the deliberate bet) | survives; `docker restart litebin-orchestrator` recovers |
| Caddy traffic burst + host pressure | n/a | alive, maybe higher latency; raise Caddy floor to 48m if this shows up |

### Pressure timeline (aggregate RAM exhaustion)

1. Free RAM depletes → kernel enters reclaim → app pages (no floors) are evicted and swapped first → apps progressively slow.
2. The stack stays resident (floors) and scheduled (weights) → dashboard and proxy keep working.
3. At the cliff, the global OOM killer takes the fattest app (score ≈ RSS, adj 0) → `restart: no` → pressure drops **permanently** until someone visits that app.
4. If another app balloons, the next fattest dies — and so on down the list. The stack is a candidate only after every app, sshd, the shims, and journald are already gone.

---

## Verification

```bash
# Applied settings
docker inspect -f '{{.HostConfig.OomScoreAdj}} {{.HostConfig.CpuShares}} {{.HostConfig.MemoryReservation}}' litebin-orchestrator
# → -300 26214 29360128

# Effective OOM score (stack should be far below any app container)
cat /proc/$(docker inspect -f '{{.State.Pid}}' litebin-orchestrator)/oom_score

# Stress test on a small host: the hog dies, the stack stays responsive
docker run --rm polinux/stress --vm 1 --vm-bytes 1024M
```

## Applying and tuning

Settings take effect at container creation. The installer's update flow regenerates `docker-compose.yml` and `docker-compose.override.yml` and recreates the master services; agent updates recreate `litebin-agent` / `litebin-agent-caddy` with the new flags. For manual changes: edit the override file and `docker compose up -d` (master), or stop/remove and re-run with the equivalent flags (agents).

Tuning knobs:

- **Caddy floor 30m → 48m (masters)** if heavy proxied traffic coinciding with host pressure shows proxy latency — Caddy's hot pages lose protection once it grows past its floor.

## Known limitations

- **`-f` bypasses the override.** Compose auto-merges `docker-compose.override.yml` only when invoked without `-f` — an explicit `docker compose -f docker-compose.yml ...` silently skips the protections. `docker compose config` in the install dir shows the merged result.
- **Rootless Docker** may not apply negative `oom_score_adj` (unprivileged processes can't decrease it). Rootful installs — the norm for LiteBin — apply it everywhere; `oom_score_adj` is a per-process knob independent of cgroup version or driver.
- **No-swap hosts** reach the OOM cliff sooner: shorter thrash window, same victim ordering, faster recovery. Swap is recommended on small hosts as the shock absorber.
- **A leaking stack component survives at the apps' expense** — that's the deliberate trade for never capping the stack (Rust + jemalloc + heap-capped Go make leaks unlikely; `docker restart` is the recovery).
