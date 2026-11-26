# Demo Mode

Per-project demo mode turns a deployed app's URL into a public demo ("try it" links in READMEs, landing pages). Two exclusive modes:

- **Shared** — one instance serves every visitor; its data resets on a schedule (default daily) or via *Reset now*.
- **Session** — every visitor gets a private clone with fresh data and a hard TTL (default 15 min); instances come from a pre-warmed pool, and at capacity visitors wait for a slot.

The project itself becomes a pure template — it never serves demo traffic and can stay stopped. Visitors keep using the project's own host (its custom domain or `{id}.{domain}`); there is one URL to share and no per-visitor DNS. Demo instances are real projects, hidden from the main grid, nested under their parent in Docker. Shipped in 0.4.26.

## Enabling

Settings → General → **Demo mode** toggle (web projects only — background projects have no public URL). The first enable snapshots the app (a brief stop, as any capture), then provisions the demo instances seeded from that snapshot, so visitors see the app as of enable-time. While demo is on:

- The card's Auto-stop/Auto-start chips are replaced by demo chips (`Demo · Shared · resets in Xh` or `Demo · Session · N/M live`); the Sleep button becomes the **Demo popover** (mode, reset interval, TTL, max instances, prewarm, banner text, bar toggle, baseline actions, disable).
- The users icon on the card opens the instances view: edit/shared instances (stop/start) and live sessions (running time, time left, end session).
- The dashboard stats bar gets an `N demos` filter chip.

Auto-stop stays active underneath — instances sleep when idle and the waker cold-starts them on return; it's just no longer operator-configurable.

## Instances

| Instance | Created | Serves | Lifetime |
|---|---|---|---|
| Edit | on enable | operator only (opened from the popover by URL) | until disable/redeploy |
| Shared | on shared enable | all visitors | until disable/redeploy; resets restore the baseline into it |
| Session | on demand (prewarm/spawn) | one visitor each | hard TTL, deleted at expiry |

The edit instance is a private copy for preparing demo data — its id is unguessable (`{parent}-l8b-edit-{rand}`), and the link is the access control (accepted trade-off: link = access, never expires). **Recapture** freezes the edit instance's state as the new baseline (the *Publish* flow); **Discard** wipes the edit copy back to a clean boot so discarded edits can't be re-published. Prewarm/pick queries exclude edit/shared instances; they never enter the pool or the capacity count.

Instances live on the parent's node — no spreading across nodes.

## Baseline

Image data is the zero-config base — no baseline means clones boot fresh and self-initialize from what the image ships. For prepared demo data (seeded account, sample content):

- **Capture as demo baseline** briefly stops the edit instance (or the parent, before the edit copy exists), copies its volumes + project files to `data/demo-seeds/{id}/project.tar` plus parent-scoped seed volumes (`l8b_demo_seed_{parent}_{n}`), then restarts. Capture time and size are shown.
- Every clone — and every shared-mode reset — starts from the seed. Without a baseline, resets wipe volumes clean.
- The baseline is frozen, not live-following: later app edits don't reach demos until re-capture. A baseline captured before an image redeploy may not match the new version's schema — app migrations handle it on boot, and the popover hints to re-capture after image changes.
- Capture/removal applies to what's already running: shared mode triggers a reset; session mode purges unclaimed pool instances so the next claim spawns from the new seed (live sessions keep running; the janitor refills the pool).
- **Remove baseline** reverts to image-only resets and wipes the edit copy (the discarded edits live there).

## Shared mode — reset engine

Visitors hit the shared instance; the parent never serves. Reset = stop/rm containers → rm volumes → restore from the baseline (or leave clean) → recreate → start. Triggers:

- Janitor tick: `last_reset_at + reset_interval_minutes` elapsed (no interval = manual resets only)
- *Reset now* (popover / `POST /projects/{id}/demo/reset`)
- Removing the baseline (seed-change propagation)

During the seconds a reset takes, the demo host serves the pending page ("Preparing your demo…") — it polls `/__demo__/status`, which reports `ready` once the app actually answers, not just a running DB row. The API returns `202 {"status": "resetting"}`; a mid-operation project (deploying/importing/resetting) answers `409` until it settles.

## Session mode — gatekeeper + pool

The demo host routes through the orchestrator's gatekeeper (hooked into the waker's host resolution), which maps a cookie (`l8b_demo`) to a session row and its instance. Request flow:

```
Host = demo host?
├─ /__demo__/* → status endpoint (bar data source + pending-page poll)
├─ cookie → session lookup (SHA-256 of the token)
│   ├─ valid, not expired → update last_seen → proxy to instance (+ bar)
│   └─ expired → "Session ended" page (start-new link)
└─ no cookie → claim under a per-parent lock (capacity check + insert atomic)
    ├─ slot free, pool has a clone → claim it (running preferred; stopped starts
    │   fast — what the pool avoids is the container-creation path)
    ├─ pool empty → spawn inline (bounded by max_instances overall) → pending page
    └─ at capacity → wait page: "Next demo slot opens in ~N min", auto-retries
        every 30 s
```

- Session TTL is **hard**: `expires_at` is absolute (issued at claim) and never extended by activity. Scale-to-zero may sleep an idle instance mid-session and the waker cold-starts it on return — the clock keeps ticking.
- The cookie is a random token, SHA-256 stored (unique index), `HttpOnly`, `SameSite=Lax`, `Max-Age` = TTL.
- At expiry the instance is deleted, not recycled: the pool refills via the janitor, bounded by max live instances (prewarm counts claimed slots).
- Direct hits on an instance host (reachable via the platform wildcard or Cloudflare records): a valid session for that instance proxies normally, the operator's edit/shared instances serve normally, anything else 301s to the demo host — a shared direct link can't bypass session limits.

All gatekeeper-served pages share one server-rendered demo theme (no app assets): pending (spinner + live status poll), wait (slot estimate + auto-retry), expired ("Start a new demo"), and a spawn-failure page.

## Demo bar

A floating bottom bar injected into `text/html` responses on the demo host: the operator's banner text (HTML allowed) plus a countdown to session end (session mode) or the next reset time (shared mode); collapses to a badge; ticks locally against the absolute expiry so there's no drift.

Injection hygiene: `Accept-Encoding` is dropped upstream (identity bodies only), and the response loses `ETag`/`Last-Modified`/`Content-Length`, CSP headers (including `<meta http-equiv>` CSP tags) and `X-Frame-Options`; `Cache-Control: no-store` and an explicit UTF-8 content type are set. Bodies over 2 MiB pass through unbarred (still streamed); injection is best-effort — a finicky app still works, and the fringe pages still communicate state.

## Cross-node

Demos work on any node — all demo traffic is force-routed through the master regardless of the install's routing mode:

- Enabling/disabling/updating a demo triggers a route sync that re-points the demo host (parent subdomain or custom domain) at the orchestrator; agents need zero demo logic.
- `master_proxy` installs already resolve demo + instance hosts via the wildcard. In `cloudflare_dns` mode, dns sync re-points the demo parent's records at the master's IP (records LiteBin manages in the platform zone only).
- Session traffic hops master → the clone's node over the existing mTLS channel (same remote-proxy path the waker uses for agent-node wakeups).
- Trade-off, accepted: the master relays demo bytes (one extra hop). Fine at demo scale, and it keeps session/capacity state in master SQLite.

## Janitor

On every 60 s tick the demo janitor:

- runs scheduled shared resets;
- expires sessions at TTL (hard teardown, retried each tick if it failed) and drops rows whose instance is gone;
- refills the prewarm pool (up to the configured count, counting claimed slots);
- self-heals missing edit/shared instances (recreated from the parent's spec, baseline data survives);
- sweeps orphans (children whose parent lost its demo config) and reaps stuck `importing` clones (10 min cap).

State lives in SQLite, so an orchestrator restart mid-session resumes cleanly — TTL enforcement continues.

## Disable & deletion

**Disable** deletes the config row instantly (the toggle snaps) and runs the heavy cleanup in the background: instances, sessions, baseline seeds. The parent is untouched and serves normally again; re-enabling rebuilds a fresh snapshot in one click.

**Deleting a demo parent** tears the demo down first — active sessions ended, seeds removed, edit/shared/pool instances deleted through the normal project path — then deletes the parent; the janitor's orphan sweep is the safety net.

Parent redeploy while demo is on: live sessions keep running their image; new clones pick up the new spec. Capabilities (`HostNetwork`, `RawPorts`, `DockerObserve`) and resource caps are inherited by every instance.

State lives in master SQLite (`demo_config`, `demo_sessions`, `demo_baseline_volumes`, plus `demo_parent_id` on projects); the schema is in the migrations (`0036_demo_mode.sql`, `0037_demo_edit_shared_instances.sql`).

## API (manage scope)

```
GET     /projects/:id/demo                -- config + baseline_captured + active_sessions + instances
PUT     /projects/:id/demo                -- enable/disable/update (max_instances 1–50, ttl 1–1440 min,
                                           --  prewarm 0–10; background projects → 400)
POST    /projects/:id/demo/reset          -- shared mode: reset now (202 {"status":"resetting"})
POST    /projects/:id/demo/baseline       -- capture + propagate to running demos (202)
DELETE  /projects/:id/demo/baseline       -- remove baseline + wipe the edit copy
GET     /projects/:id/demo/sessions       -- sessions (last 100) + long-lived instances
DELETE  /projects/:id/demo/sessions/:sid  -- end session + tear down its instance
-- visitor-facing, on the demo host only:
GET     /__demo__/status                  -- cookie-gated state: ready/pending/expired/none/error
                                          --  (+ expires_at; shared mode reports resets_at)
```

## Known gaps

Open items, current as of 0.4.26:

- [ ] **Parent card stats ignore demo instances** — CPU/mem/disk tiles reflect the (usually stopped) parent only; they should sum the demo children, and the instances view could show per-instance rows.
- [ ] **Parent redeploy doesn't refresh edit/shared instances** — they keep the old image and `.env`; pool spawns pick the new spec. Env/limit changes likewise reach new instances only.
- [ ] **Migrating a demo parent** leaves its instances and seed volumes on the old node — migration neither blocks demo parents nor moves the children.
- [ ] **No per-IP rate limit on session claims** — the planned fixed-window limiter (e.g. 5/min per IP on the demo host) isn't implemented, so scripted instance farming is unblunted.
- [ ] **No dedicated "resetting" page** — during a shared reset visitors get the generic pending page; the API does report `{"status": "resetting"}`.
- [ ] **Custom demo domains** in zones LiteBin doesn't manage need a manual re-point to the master; there's no dashboard hint for it yet.

Accepted limitations (design decisions, documented):

- [x] Bar injection is best-effort: pages over 2 MiB pass through unbarred; SPAs that rewrite `<body>` can lose the bar on client-side navigation — fringe pages still communicate the time.
- [x] Duplicating/exporting a demo parent yields a plain project — demo config and baseline seeds don't travel.
- [x] Edit-instance access is the unguessable URL — link = access, never expires.
- [x] No node-level demo budget — N demo projects × `max_instances` each is operator discipline (a global cap was explicitly rejected in design).

Related: `access-control.md` (same caddy route surface; password protection could later gate demos), `template-catalog.md` (the baseline mechanism doubles as template seeding).
