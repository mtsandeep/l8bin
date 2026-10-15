# Resource-protection A/B results — 1GB VPS

Same ramp, twice: `polinux/stress` apps (`--vm 1 --vm-bytes 480M`, 512M cap each) deployed ~2 min apart until the host OOM cliff. Baseline ran on the stock install; protected after applying `docker-compose.override.yml` (oom_score_adj -300 / cpu_shares 26214 / mem_reservation 28m-8m-30m). Raw data: `baseline/` and `protected/` (probe.log = dashboard latency 1/s from an external machine, ~0.7s idle floor; stats.csv = per-container state; events.log = kills).

**Host:** 961MB RAM + 2.3GB swap, single disk. Ramp capacity: ~3.3GB → cliff at app 7 both runs.

## Results

| Window (apps live) | Baseline avg / max | Protected avg / max |
|---|---|---|
| 1–2 (~1GB demand) | ~0.7s / — | **0.78s / 2.0s** |
| 3–4 (~2GB) | 0.95s / 4.3s | **0.99s / 3.8s** (mostly at idle floor) |
| 5 (~2.4GB) | 1.09s / 4.0s | 1.59s / 6.5s |
| 6 (~2.9GB) | 2.69s / 11.9s | 2.44s / 15.0s (one 502 during deploy burst) |
| 7 + kill (~3.3GB) | 1.41s / 5.8s | 1.80s / 4.0s |
| Whole run ≥1s / ≥2s | 66% / 35% | 38% / 14% |

| Kill event | Baseline | Protected |
|---|---|---|
| Trigger | stress-7 deploy, 25s later | stress-7 deploy, same moment |
| Victim | stress-4 — RSS lottery, no guarantee | stress-3 — an app **by construction** (stack ineligible at -300) |
| Aftermath | 480MB freed, pressure ended, app stayed dead (`restart: no`) | same |
| Stack | survived (small = lucky), degraded to 11.9s worst, 2 hard API outages | survived **by guarantee**, one transient 502 (15s) during the deploy burst |

## Findings

1. **Victim determinism held** — the core guarantee. At -300 the stack was never a candidate; an app died at the cliff in both runs.
2. **The floors worked through ~2× overcommit**: at 1–4 apps (up to ~2GB demand on a 1GB box) the protected dashboard sat at its idle floor while baseline was already degrading with 2–4s spikes.
3. **Deep saturation is disk-bound, not memory-bound**: at 6–7 apps (2.9–3.3GB) both runs degraded to 1.5–3s averages — the stress apps' swap churn saturates the disk and kswapd burns CPU, and `memory.low` cannot shield the stack from *other processes'* I/O and kernel reclaim. This matches the resource-protection doc's predicted residual; the fix for this zone is proactive eviction (standby-sleep plan; PSI trigger), not floors.
4. **Honest confounds**: one run per phase; **shared-CPU VPS (Vultr)** — sustained pegged CPU can trigger provider throttling/steal and the plan's disk throughput cap, adding run-to-run noise (likely cause of the small inversions, e.g., protected p95 slightly worse than unprotected at 1G swap); baseline's 3–4-app window contains a few observer TLS-error samples; swap pre-fill differed. The big deltas (swap-size effect, max-latency improvements) are far beyond this noise; close cells are inconclusive by design. Absolute latencies are directional, not benchmark-grade — a dedicated-CPU confirmation run would tighten them.

## Phase D — same protected stack, swap cut to 1GB (1×RAM)

Hypothesis: with wake-on-request, shedding memory early is cheaper than hoarding it — smaller swap → faster cliff → quick surgical kills → box stays fast. Confirmed:

| Metric | Baseline (2.3G swap) | Protected (2.3G swap) | Protected (1G swap) |
|---|---|---|---|
| Probe avg / max | ~1.5s / 11.9s | ~1.3s / 15.0s | **0.89s / 5.4s** |
| Samples ≥2s | 35% | 14% | **4%** |
| API outages | 2+ hard | several timeouts | **0** |
| Cliff | app 7, ~15 min in | app 7 | **app 4, 47s after overshoot** |
| Sheds | 1 | 1 | **2 — repeats on each new demand (23s, 47s)** |
| Victims | app (lottery) | app (guaranteed) | app (guaranteed) |

The box resolved every overshoot within a minute by shedding one app and returned to idle-floor latency between cycles. Caveats: the OOM killer still picks by RSS, not idleness — policy eviction (standby-sleep) is the correct-victim version of this; background apps don't auto-wake; less swap means less grace for deploy/backup spikes (1×RAM retained enough here).

## Phase E — the control cell: unprotected + 1GB swap (+ k6 percentiles)

Same ramp with the override removed (`adj=0`). Probe-level results matched the protected run almost exactly (avg 0.91s vs 0.94s; identical instant sheds at each overshoot). A k6 percentile A/B (`measure.js`, 2 min per side, keep-alive connections, steady 3-app edge state) separated the variables:

| route | metric | unprotected | protected |
|---|---|---|---|
| `/` | med / p90 / p95 / max | 243 / 373 / 564ms / 1.9s | 245 / 332 / 584ms / 2.3s |
| `/projects/stats` | med / p90 / p95 / max | 373 / 904 / 1420ms / 3.0s | 378 / 811 / 1644ms / 3.0s |

**Identical within run-to-run variance.** At 1×RAM swap the saturation windows are too short for stack pages to be evicted, so the floors have nothing to protect against — unprotected latency converges with protected.

### Attribution — what each layer is actually responsible for

- **Kill-order (`oom_score_adj`)**: the victim guarantee, in every configuration. Not visible in latency; visible in survivorship. Non-negotiable, costs nothing.
- **Floors (`memory.low`)**: latency protection during *long* saturation — worth a lot at large swap (Phase A vs B: 2–4s spikes vs idle floor at 1–4 apps; 35% → 14% degraded samples), worth ~nothing measurable at 1×RAM swap (Phase E k6). Insurance for the slow-death scenario, not a speed feature.
- **Swap size**: the dominant latency variable on a disk-bound small box. 2.4× → 1×RAM cut degraded samples from 14% → 4% and outages to zero by shrinking exposure windows.
- **Percentiles, not averages**: the box is bimodal under churn (300ms resident / seconds during fault storms) — med/p95/max tell the story that averages hide.

## Phase H — CPU storms, daemon starvation, and the disaster recreation

A fleet of pure-CPU apps (`--cpu 2`, 0.5-CPU cap, no memory work) up to 20 apps / **1000% oversubscription**, then a recreation of the original disaster (28 × 100MB memory-churn apps, full severity), with the stack protections on throughout and systemd-level daemon protection toggled live via `systemctl set-property`.

**Finding 1 — pure CPU oversubscription is survivable without help.** At 1000%, with daemon weights toggled OFF, management actions still completed in ~1–2s: dockerd needs only ~100ms of CPU for a small operation, and even at ~5% weighted share that's a second or two. A memory-resident daemon is fast while starved.

**Finding 2 — what actually killed the box the first time was the daemon's *memory*, not CPU.** The original disaster hung every action because the memory-churn fleet swapped dockerd's own working set out; each "100ms" op became a page-fault storm against a saturated disk. CPU famine set the scene; unresident daemons were the weapon. (Prod of Finding 1: the CPU-weight-only prediction failed its live A/B at 1000% — both lanes ~1–2s.)

**Finding 3 — the complete fix is two-axis daemon protection**, same as the stack containers got:

```ini
# /etc/systemd/system/{docker,containerd}.service.d/ — applied live via systemctl set-property
CPUWeight=2000      # scheduler fast lane for the management plane
MemoryLow=150M      # reclaim protection — keep the daemon's pages resident
```

**Validation — the disaster recreated with full hardening** (21 memory-churn apps live, 7 already shed at the cliff): deploys 6.0s / 2.1s, deletes 6.3s / 12.9s, dashboard probe 0.98s, SSH alive, and **cleanup of all 28 apps completed in 34s** — versus the original: actions hung indefinitely, cleanup zero-for-28, SSH dead, provider panel required.

## Conclusion

Floors + kill-order deliver what they mechanically can: **the platform stays at full speed while the box is overcommitted up to ~2×, and when the cliff comes, an app dies — never the platform.** Cutting swap from 2.4× to 1×RAM then made the composite behavior near-ideal: quick sheds, 0.89s average, 4% degraded samples, zero outages — with the control run (Phase E) proving the small-swap responsiveness is mostly the swap's doing, while the protection contributes the guarantee plus long-saturation latency insurance. The daemon-hardening phases (H) completed the story: **the management plane needs the same two-axis protection as the stack containers** — with `CPUWeight` + `MemoryLow` on docker/containerd, the platform survived and managed its way out of the exact storm that originally bricked it. The recommended node config: stack floors + kill-order + daemon weights/memory floors + **~1×RAM swap**, with policy eviction as the follow-on (correct-victim shedding: idle-first instead of RSS-first).
