# Memory footprint

What the platform uses, why, and how to measure it. Numbers measured 2026-10-11 on 0.4.5. Re-measure with the cookbook before quoting numbers publicly.

## Test hosts

| | 8GB VPS | 512MB VPS |
|---|---|---|
| Role | master | master |
| Projects | 9 — 7 local, 2 on an agent node | 5 |
| App containers | 7 (4 always on) | 8 |
| vCPU | 4 | 1 |
| RAM / swap | 7.8Gi usable / 4G swapfile | 458Mi usable / 512M swapfile |
| Disk | 251G | 8.9G |
| OS / virt | Debian 13 · cgroup v2 · KVM | Debian 13 · cgroup v2 · Hyper-V |
| THP | `always` | `never` |

CPU count is the root-cause variable: pre-fix jemalloc ran `4 × CPU` arenas (16 vs 4). Swap is a disk-backed file on both (not zram), so swapped-out MB are RAM genuinely saved. THP `always` on the 8GB box can slightly inflate its RSS there.

## The model

| Component | Behavior |
|---|---|
| Orchestrator | ~30MB live working set; RSS depends on allocator config (below) |
| Dashboard | 2–5MB, system allocator |
| Caddy | `GOMEMLIMIT=35MiB` on hosts <2GB RAM (install.sh); Go defaults elsewhere — floats to ~60MB as GC headroom, by design |
| Agent | Same allocator setup as orchestrator |

## Allocator tuning (0.4.6+)

Orchestrator and agent images ship `_RJEM_MALLOC_CONF=dirty_decay_ms:1000,muzzy_decay_ms:0,narenas:2` (override per host via compose env or `.env`).

Why: jemalloc's defaults (`narenas = 4×CPU`, `dirty_decay_ms = 10000`) target throughput. The orchestrator's workload is bursty (boot config builds, per-poll TLS clients, deploy buffering) and idle between bursts — freed pages scattered across per-CPU arenas get pinned by surviving allocations, so RSS sat at the historical high-water mark: 77MB idle on the 8GB VPS for ~34MB live. `narenas:2` does most of the work; the 1s decay drains fully-empty pages while batching purges. `decay:0` was tested — only ~2MB better, more syscalls.

- The variable must be `_RJEM_MALLOC_CONF` — tikv-jemalloc ignores plain `MALLOC_CONF`.
- Shipped builds compile jemalloc without stats/prof, so `malloc_stats_print` prints config only (see cookbook).
- Fragmentation-pinned pages only clear on restart; platform updates do this routinely.

## Measured — 8GB VPS

| State | Before (jemalloc defaults) | After (tuned) |
|---|---|---|
| Boot → idle plateau | 35.8 → 77.4MB | 28 → 33.6MB |
| Dashboard open | 93.6–97MB | 37.8–39.6MB |
| Deploy | +20 spike, residue stuck | +22 spike, drains in seconds |
| First-deploy lifetime floor | ~94MB+ | 52–55MB (later deploys reuse it — no accumulation) |

Ruled out as leaks (all measured flat over hours): per-request paths (2000-curl test), heartbeat, periodic status sync, janitor, activity tailer, dashboard stats polls, app count, DB size, GitHub sync, Litestream.

## Measured — 512MB VPS (swap in active use)

| State | Pre-fix | Tuned |
|---|---|---|
| Orchestrator idle (RSS+swap) | ~20MB (swap untracked) | 17–20MB |
| All 5 projects running (11 containers) | ~26–33MB | 26–30MB |
| Post-boot live minimum | n/a (never purged) | 12.3MB |
| Caddy (heap-capped) | 23–39MB | unchanged |

On small boxes where swap is actively used, RSS alone misleads: the kernel opportunistically swaps cold pages (observed oscillating 2–11MB anti-phase with RSS; harmless budget-swapping, `vmstat` si/so 10–40kB/s). Always compare RSS+VmSwap, or cgroup `memory.current + memory.swap.current`. The tuning helps modestly at 1 CPU (jemalloc's 4 arenas were already near-correct there); the large effect is on multi-core masters.

## Cookbook

```bash
# RSS sampler (pgrep -x — pgrep -f also matches `docker logs -f litebin-orchestrator`)
for i in $(seq 15); do printf "%s  " "$(date -u +%T)"; \
  awk '/VmRSS/{printf "%.1f MB\n", $2/1024}' /proc/$(pgrep -x litebin-orchest)/status; sleep 20; done

# docker stats (what users reproduce) vs cgroup anon/file split
docker stats --no-stream
pid=$(docker inspect -f '{{.State.Pid}}' litebin-orchestrator)
cg="/sys/fs/cgroup$(tail -1 /proc/$pid/cgroup | cut -d: -f3)"
awk '$1=="anon"||$1=="file"{printf "  %s %.1f MB\n",$1,$2/1048576}' $cg/memory.stat

# total footprint (RSS+swap) — swap is configured on both test hosts; record both
grep -E 'VmRSS|VmSwap' /proc/$(pgrep -x litebin-orchest)/status
awk 'NR==1{r=$1} NR==2{s=$1} END{printf "ram=%.1fMB swap=%.1fMB\n",r/1048576,s/1048576}' \
  $cg/memory.current $cg/memory.swap.current

# verify the allocator env was read
p=$(pgrep -x litebin-orchest); cp /proc/$p/root/app/litebin-orchestrator /tmp/orch
gdb -batch /tmp/orch -p $p -ex 'call (void) _rjem_malloc_stats_print(0, 0, 0)' -ex 'detach'
docker logs litebin-orchestrator --since 2m 2>&1 | grep -E 'opt.narenas|opt.dirty_decay'
```

Gotchas: `free`'s buff/cache is reclaimable page cache, not usage; cgroup `memory.current` includes evictable file cache and misses text pages charged to containerd (docker stats is the number users reproduce); `grep -E '^(Rss|Pss):' /proc/$(pgrep -x litebin-orchest)/smaps_rollup` when shared pages must be counted fairly.

### Measuring from a fresh start

For a clean baseline, close all dashboard tabs first (an open tab polls stats and adds a few MB), then restart and watch the boot curve:

```bash
# fresh containers (or `docker restart litebin-orchestrator` if recreated containers aren't needed)
cd /opt/litebin && docker compose up -d --force-recreate

# capture the boot curve — expect a spike in the first minutes, then a flat plateau
for i in $(seq 15); do printf "%s  " "$(date -u +%T)"; \
  awk '/VmRSS/{printf "%.1f MB\n", $2/1024}' /proc/$(pgrep -x litebin-orchest)/status; sleep 20; done

# platform totals once the plateau is reached (~5 min)
docker stats --no-stream
```

For a whole-box baseline (empty page cache and swap), `reboot` the VPS instead and run the same sampler once it's back. Two caveats when quoting fresh-start numbers: Caddy reads low right after a restart and floats to its steady state over the following hours on ≥2GB hosts, and the orchestrator's post-boot plateau is its idle number — the first deploy adds a one-time high-water (see the 8GB table).
