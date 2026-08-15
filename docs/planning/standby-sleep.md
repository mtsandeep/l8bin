# Standby Sleep (Pressure-Based Eviction)

## Context

Current sleep is timer-based: an app with `auto_stop` enabled is put to sleep after X minutes without traffic, unconditionally. This throws away a warm app even when the node has RAM to spare. Observed in practice on the 512MB test VPS (11 services / 5 apps): all apps crossed their idle threshold and went to sleep while the node sat at ~40% free RAM — free memory, paid for, doing nothing. Low-traffic apps (the target user profile) pay the cold-start cost on every wake for reclamation the node never needed.

The fix is to decouple **eligibility** from **reclamation**: mark apps as eligible once idle, but only actually reclaim (sleep) when the node needs the resources. This is the same shape as OS page-cache eviction (idle pages stay mapped until pressure) and Kubernetes BestEffort pods (run on leftovers, yield first under node pressure) — at a scale where neither machinery is warranted.

## Current Pattern (to replace)

```
idle_for > threshold  →  sleep immediately, always
```

## Target Pattern

**Standby state** — an app is `standby` when running and idle past its threshold:

```
standby(app) = running(app) && idle_for(app) > app.threshold
```

Derived, not persisted — computed from last-traffic timestamp already tracked. Status ladder becomes `Running → Standby → Sleeping`.

**Reclamation policy** — the agent watches node RAM against two marks (global/node setting):

- High-water (default 80%): sustained for the settle window (~30–60s) → sleep standby apps per ranking until…
- Low-water (default ~65%): stop evicting.

Instant readings don't trigger — apps often spike at startup and release; evicting on a 3-second transient is flapping with extra steps.

**Sleep execution order** — ranking among standby apps, evict best-score first:

```
score = idle_minutes × current_rss / recent_wake_count
```

Largest idle footprint per expected re-wake cost — an app that wakes hourly shouldn't be first out despite being idle. Apps with `auto_stop` off are never evictable (reserved, not best-effort). No post-wake grace period is needed: standby eligibility (idle > threshold) already implies the app has been quiet since its last request, and the ranking's `recent_wake_count` term covers periodic-ping apps that would otherwise cycle wake → standby → evict.

**Wake path (hybrid admission)** — the 20% headroom between water marks is the admission budget:

- **Fast path (common):** app's expected footprint (historical RSS from stats) fits the reserve → wake immediately without waiting; swap absorbs startup transients. After start, if pressure persists past the settle window, evict from the ranked list as normal.
- **Slow path (rare, big apps):** expected footprint exceeds the reserve → evict from the ranked list first, then wake. The waker already holds the request during wake; slightly longer hold is acceptable here.

**Placement** — pressure decisions are node-local truth, so they belong on the agent (which already owns the waker and activity tracking). The orchestrator/janitor keeps computing idle eligibility; this preserves the agents-survive-master-down property.

## Scope

| Piece | Where | Notes |
|---|---|---|
| Standby derivation + status surfacing | litebin-common `types.rs`, dashboard badge | Derived state; no `ProjectStatus` enum/DB change |
| Pressure watcher | agent | RAM high/low-water check on a window; node-level setting with global default (80%) |
| Eviction ranking | agent | Uses existing per-app stats; wakes tracked via existing wake path |
| Hybrid wake admission | `agent/src/routes/waker/` | Fast path default; slow path when expected RSS > reserve |
| Settings | orchestrator routes + dashboard | Standby threshold semantics; pressure marks |
| Docs | `failure-model.md`, `user-flows.md`, README | New row: node under memory pressure → what the user experiences |

## Considerations

- **CPU is not a trigger (for now).** Idle apps burn ~0 CPU; evicting them frees no CPU. Trigger is RAM only. Value split to state honestly in docs: for small apps, standby's win is mostly avoiding cold starts; for chunky apps (100MB+) it's real memory packing.
- **CPU as a ranking signal — revisit at implementation.** Standby means no traffic, not no CPU: an app can be idle from HTTP's perspective yet still burn cycles (background loops, workers, misbehavior). Those are the only standby apps whose eviction frees CPU, so CPU usage could join the ranking (or act as an additional eviction condition when it's affecting other apps). Needs care: legitimate background work with no HTTP traffic would be killed — decide the exact rule at implementation time.
- **Semantics change: "may" sleep, not "will".** "Sleep after X min" currently means the app *will* sleep at X; standby makes it *may* sleep at X — reclamation happens only under pressure. The behavior changes outright (no compatibility mode); the requirement is clarity in wording: dashboard labels, settings help text, and docs must say "may sleep after X minutes of no traffic" so users understand the contract.
- **Reservations accounting.** Sum of never-evictable apps' expected footprints should fit the node; everything else is best-effort. No hard enforcement needed at this scale, but document the expectation.
- **Interplay with `memory_limit_mb`.** Limits contain a misbehaving app's blast radius (per-container OOM kill); standby eviction handles node-level packing. Two separate mechanisms, deliberately — same split as K8s limits vs node-pressure eviction.
- **Eviction cap.** Bound evictions per time window as a second flap guard beyond the hysteresis marks.
- **Swap.** Present swap (e.g., 512MB on the test node) makes the optimistic fast path much safer — transient overshoot degrades into swap, not OOM. Document as recommended, not required.

## Priority

Medium-high — directly increases what a small node usefully runs (the product's core promise), and the inputs (idle timestamps, per-app RSS, waker) already exist. Sequenced after backup/migration/preview environments per the roadmap; no new infrastructure required.
