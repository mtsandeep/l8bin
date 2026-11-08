> **Status: NOT STARTED — design agreed 2026-11-07** · no code written yet

# Demo Mode: Shared Instance + Per-Session Instances

Per-project demo mode for sharing deployed apps as public demos ("try it" links in GitHub READMEs, landing pages). Two independent modes. Marketing feature: visitors try the app, not the LiteBin dashboard.

A project (e.g. `helloworld.example.com`) gets a demo toggle. Demo traffic keeps using the project's own host — its custom domain or `{id}.{domain}` — no extra DNS.

---

## Two Modes

| | Shared (Mode A) | Session (Mode B) |
|---|---|---|
| **Instances** | 1, used by all visitors | N clones, one per visitor session |
| **Data** | Everyone shares; wiped on reset | Each session starts with empty volumes |
| **Lifecycle** | Reset on interval (default daily) | Clone on arrival, hard TTL teardown |
| **Capacity** | Unlimited visitors | `max_instances` (default 5); over limit → wait page |
| **Gatekeeper** | None (project served as-is) | Orchestrator proxies per-session |

Both modes show a floating bottom bar (see below). Modes are exclusive: enabling one disables the other.

---

## User Experience

### Operator

```
1. Deploy project normally (image or compose)
2. Card settings popover → General tab → enable "Demo mode"
3. Demo popover on the card — choose mode:
   Shared:   reset interval (default 1440 min), banner text
   Session:  TTL (default 15 min), max instances (default 5), prewarm (default 1), banner text
4. Share the project URL anywhere
5. Card shows demo chips in place of auto-stop/auto-start; Mode B adds the instances expander
```

### Visitor (Mode B)

**One domain, no per-visitor URLs.** Visitors only ever see the demo host (`helloworld.example.com`); the gatekeeper maps cookie → instance. Instance subdomains are infrastructure-only and never visitor-facing: a direct hit on `{cloneid}.{domain}` 301-redirects to the demo host, so traffic can't bypass session limits by sharing a direct link.

```
1. Hit helloworld.example.com → no cookie
2. Claim (atomic): pre-warmed clone available → claimed + started (fast even if the
   janitor stopped it), cookie issued, proxied
   No clone ready → pending page ("Starting your demo…") — creation runs in background,
   page polls until the clone exists, then the session cookie is issued
   At capacity → wait page: "5/5 sessions in use, earliest ends in ~4 min",
   auto-retry with jitter (no all-waiters-at-once herd)
3. Session live: app proxied transparently + demo bottom bar (countdown)
4. TTL reached → expired page ("start a new one") → teardown; in-flight connections
   are hard-killed
```

Session TTL is **hard**: 15 min from issue, never extended by activity. Within a live session, scale-to-zero may sleep an idle instance and the waker cold-starts it on return — resource saving only, the clock keeps ticking.

---

## Core Decisions (agreed)

1. **Demo instances are real projects** created via the copy-engine machinery, with `demo_parent_id` set. Free: routing, caddy config, volumes, networks, resource caps + capabilities inherited, per-project stats, scale-to-zero, existing delete path. Main grid filters them out; they render nested under the base project.
2. **Base project stays pristine** — never serves visitor traffic in Mode B. It is the template spec (can even stay stopped; cloning reads the spec from DB). Pre-warm is a separate clone; when claimed, a replacement spawns in the background (demo-kuma `pool.ts` pattern). Prewarms are **pre-created** containers — the janitor is free to auto-stop them (stop/start is fast); what the pool exists to avoid is the container-*creation* path on the hot path.
3. **Clone data — two seed sources**: default is fresh empty volumes (definitions only; the app self-initializes from what the image ships). Optionally a **demo baseline** — the operator freezes the base project's current data once, and every clone (and every Mode A reset) starts from that snapshot (see "Demo baseline").
4. **No fallback mixing**: at capacity, visitors wait — never routed to a shared instance.
5. **Bottom bar**: floating bar injected by the gatekeeper with operator-configurable text (set from project settings, e.g. "Demo — some features are locked") + countdown to session end. In Mode A the right side shows "resets at HH:MM" instead of a countdown.
6. **Creation never happens on the hot path**: claiming a prewarm (even one the janitor stopped) is a fast container start. Clone creation runs in the background at refill time; a visitor arriving while a clone is being created sees the pending page.
7. **Hard teardown at TTL**: in-flight connections are killed at expiry — no drain grace. Slot claims are atomic: two simultaneous first-hitters, one slot → exactly one wins.
8. **Cross-node by design**: demo hosts always route through the master (see "Cross-node"); clones run on the parent's node, local or agent.

---

## Data Model

```sql
-- Mark demo instances (children); NULL = normal project
ALTER TABLE projects ADD COLUMN demo_parent_id TEXT REFERENCES projects(id);

CREATE TABLE demo_config (
    project_id            TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode                  TEXT NOT NULL,              -- 'shared' | 'session'
    max_instances         INTEGER NOT NULL DEFAULT 5,   -- Mode B
    session_ttl_minutes   INTEGER NOT NULL DEFAULT 15,  -- Mode B, hard TTL
    prewarm               INTEGER NOT NULL DEFAULT 1,   -- Mode B
    reset_interval_minutes INTEGER,                     -- Mode A; NULL = manual reset only
    banner_text           TEXT NOT NULL DEFAULT 'Demo environment — data resets periodically',
    bar_enabled           INTEGER NOT NULL DEFAULT 1,
    last_reset_at         INTEGER,                      -- Mode A anchor
    updated_at            INTEGER NOT NULL
);

CREATE TABLE demo_sessions (
    id                  TEXT PRIMARY KEY,
    parent_project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    instance_project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    token_hash          TEXT NOT NULL,                -- SHA-256 of cookie value (deploy_tokens pattern)
    created_at          INTEGER NOT NULL,
    expires_at          INTEGER NOT NULL,             -- created_at + TTL, never extended
    last_seen_at        INTEGER NOT NULL,
    status              TEXT NOT NULL DEFAULT 'active' -- 'active' | 'expired' | 'ended'
);
```

---

## How It Works

### Mode A — reset engine

Reset = stop/rm containers → rm volumes → recreate containers from stored spec (same project id, same routing) → start. This is "duplicate in place, discard the original" — reuses copy-engine recreation primitives; volumes are restored **from the baseline if one is captured**, else wiped clean.

- Triggers: janitor tick (`now - last_reset_at >= reset_interval`), "Reset now" button, demo disable (one final reset)
- During reset (seconds) the demo host serves a brief "resetting" page
- Routing: for a local-node project the traffic path is unchanged until the gatekeeper lands (step 5); from then on both demo modes route the demo host through the gatekeeper — which is also what makes Mode A work on agent nodes

### Mode B — gatekeeper + pool

The demo host routes to the **orchestrator**, not the project — same host-interception pattern the waker already uses (dashboard/poke/wake hosts). Enabling/disabling demo mode triggers a route sync.

Request flow (`waker/handler.rs` pattern, new demo branch):

```
Host = demo host?
├─ reserved path /__demo__/* → session.json / bar assets / status poll / fringe pages
├─ cookie → session lookup
│   ├─ valid + not expired → update last_seen → proxy to instance project
│   │   (start if the janitor stopped it — fast, no creation)
│   └─ expired → expired page (link: start new session)
└─ no cookie → capacity check (atomic claim: one tx = check slots + mark claim)
    ├─ slot free + pre-warmed clone → claim + start, issue cookie, proxy (fast path)
    ├─ slot free + clone creating → pending page (polls /__demo__/status; on ready the
    │   server creates the session row + issues the real cookie; on failure → error page)
    └─ full → wait page (live: N/M in use, earliest ends in ~X min, jittered auto-retry)
```

Proxy requirements: streaming-safe (WebSocket/SSE passthrough untouched), `Host` set to the demo host (matches what the visitor's browser sees), `X-Forwarded-*` carrying instance identity. Target for multi-service projects = the primary web service (same rule as normal host routing). Instance hosts route to the gatekeeper as well — valid session cookie → proxy, otherwise 301 to the demo host.

Injection hygiene (bar path): strip `ETag`/`Last-Modified` (a 304 would serve a bar-less page after a bar-ful one), strip CSP headers **and** rewrite `<meta http-equiv="Content-Security-Policy">` tags, force identity encoding to the upstream.

### Cross-node

Demo works on any node — all instances live on **the node the project was deployed to** (no spreading across nodes), and demo traffic is force-routed through the master regardless of the install's routing mode:

- **Forced master routing**: enabling demo re-points the demo host (parent's subdomain or custom domain) at the **master's** public IP — a per-host override; the install's routing mode is untouched. `master_proxy`: already true (wildcard). `cloudflare_dns`: dns sync changes the parent's record target to master (only for records litebin manages in the platform zone; a custom demo domain in an external zone needs the operator to re-point it — hint in the dashboard). This is what sidesteps cloudflare-mode complexity: no per-node demo routing, no agent-side logic.
- **No DNS records for clones**: instances are infrastructure-only — dns sync creates nothing for them (per-session record churn would be slow and racy). Reachability: `master_proxy` wildcard resolves clone hosts → gatekeeper 301s to the demo host; `cloudflare_dns` clone hosts simply don't resolve publicly.
- **Master caddy** fronts the demo host with on-demand TLS and routes it to the gatekeeper — both routing modes, both demo modes.
- **Upstream hop**: session → clone project → its node. Local node: existing waker path (direct container upstream). Agent node: hop over the existing master→agent mTLS channel (config push proves it exists) or the node's caddy with `Host: {cloneid}.{domain}` + internal auth header — verify which during step 5.
- **Clone creation** on agent nodes reuses the existing deploy-to-node path (migration already creates projects on remote nodes).
- Trade-off, accepted: the master relays demo bytes (one extra hop, master bandwidth). Fine at demo scale — and it keeps session/capacity state in master SQLite with agents needing zero demo logic.
- **Record retargeting is already in place**: dns sync compares record content before skipping and re-points drifted records (fixed in 0.4.20 — the old bug silently kept stale IPs when a record's name already existed), with zone-aware and forced re-pointing added since. Demo only needs `desired_dns_records` to target the master IP for demo hosts.

Session cookie: random token, SHA-256 stored, HttpOnly, SameSite=Lax, expires with the session.

### Clone

Extracted from duplicate internals (`copy_engine/`): allocate fresh project id via `ids.rs` (dodges reserved subdomains + existing ids), copy service spec (image tags shared, no copy), create volumes per definitions (**empty**, or seeded from the baseline), create network, register routing, set `demo_parent_id`. Base project state irrelevant (stopped is fine). Multi-service/compose projects supported.

### Seamless model (agreed 2026-11-08 — target UX; supersedes the enable-time data-mode choice)

Invariant the user holds: **"Visitors always see your last snapshot. The edit copy is how you make the next one."**

- **Enable (one click)**: snapshot the parent (brief stop, as any capture) → create edit + shared/pool instances **seeded from it** → visitors see the app as of enable-time. No data-mode decision at the door.
- **Recapture** (from the edit instance) = the next snapshot; demos reset to it.
- **"Start from scratch"** (small opt-in action): drops the snapshot → visitors get fresh image-state apps (the old "Deployed data", now the exception for self-seeding apps, not a default choice).
- **Disable = delete everything**: instances, sessions, snapshot (seeds incl.), config. Parent untouched and serves normally. No paused/resume state — re-enabling rebuilds a fresh snapshot in one click.
- Reset is phrased **"Restore demo to snapshot"**. Shared/Session stays a real choice; TTL/max/banner unchanged.

Build deltas from current code: auto-capture in the enable path before instance creation; popover data section becomes snapshot language + start-from-scratch action; disable also removes seed volumes/tar (today rows cascade but seed data lingers); resolves the "edit instance created empty" and "baseline dies on disable" gaps by design (both become moot — edit is seeded at enable, snapshot deletion is intended).

### Instance model (agreed 2026-11-08, supersedes the base-cookie bypass)

Everything is an instance; the parent is a pure spec template — never serves traffic, never edited, can stay stopped. `demo_config` records `edit_instance_id` (+ `shared_instance_id` for shared mode). Edit instances get unguessable ids (`{parent}-edit-{rand}`) — the popover opens them by URL, no auth exchange (accepted trade-off: link = access, never expires).

| Instance | Created | Serves | Lifetime |
|---|---|---|---|
| Edit | on demo enable | operator only (popover URL) | until disable/redeploy |
| Shared | on shared enable | all visitors | until disable/redeploy; resets restore baseline into it |
| Session | on demand (prewarm/spawn) | one visitor each | hard TTL |

- **Recapture captures from the edit instance** (first capture with no edit instance yet falls back to the parent).
- Reset engine retargets the shared instance; gatekeeper shared mode proxies to it (bar included).
- Edit + shared instance hosts: exempt from the instance-host 301; no bar.
- Prewarm/pick queries exclude edit/shared ids; disable tears down all instances.
- Parent redeploy → recreate edit + shared instances from the new spec (baseline data survives).

### Demo baseline (optional seed)

Image data is the zero-config base — no baseline means clones boot fresh and self-initialize. For prepared demo data (seeded account, sample content):

- Operator preps the running base app, then hits **"Capture as demo baseline"** in the demo popover
- Capture = brief base stop → volumes + project dir copied to a seed store under `data/demo-seeds/{id}` (same volume-copy machinery as duplicate; base restarts after) → capture time + size shown
- Every clone starts from the seed; **Mode A resets restore to the seed** too (clean volumes when no baseline exists)
- Frozen, not live-following: later base edits don't reach demos until re-capture; clone creation never touches the running base (keeps creation off the hot path)
- Re-capture anytime to update; removing the baseline reverts to image-only
- Caveat surfaced in the popover: a baseline captured before an image redeploy may not match the new version's schema — app migrations handle it on boot (same as any restore); hint to re-capture after image changes

### Teardown

Janitor sweep: sessions past `expires_at` (or `ended`) → delete instance project through the existing delete path (containers, volumes, networks, caddy route) + remove session row. Teardown is a **hard kill** — no drain grace. A freed slot triggers a background prewarm refill (creation off the hot path). Partial/failed clones are cleaned immediately when the creation task reports failure, not just by the reconcile loop. Reconcile loop also orphans-checks: instance projects with no live session row get cleaned (crash recovery after orchestrator restart; sessions persist in SQLite so this is a safety net).

### Demo bar (injection)

For `text/html` non-streaming responses on the demo host: strip `Content-Security-Policy` + `X-Frame-Options`, inject `<script>`/`<style>` before `</body>` (fallback: `</html>`, then append; best-effort). Force identity encoding to upstream (or decompress/recompress) so injection is safe.

- `/__demo__/session.json` → `{ seconds_left, expires_at, banner_text, mode, reset_at? }` (cookie-gated; bar ticks locally against absolute expiry — no drift)
- Bar shows configured banner text + countdown (Mode B) or reset time (Mode A); on expiry flips to "session ended — reload"
- Non-HTML responses untouched; if injection fails on a finicky app the session still works — fringe pages (interstitial/expired) still communicate the time
- Collision risk on `/__demo__/*` with app routes is accepted and documented

### Fringe pages

Every gatekeeper-served page shares one server-rendered demo theme (LiteBin styling, no app assets), reusing `banner_text` where sensible:

| Page | When | Content |
|---|---|---|
| Pending | clone creating | "Starting your demo…", live status poll |
| Wait | at capacity | N/M in use, earliest end estimate, jittered auto-retry |
| Expired | TTL hit | "Session ended" + start-new link |
| Error | clone failed | what failed + retry |
| Resetting | Mode A reset in progress | brief, auto-reloads |

---

## API Endpoints

```
GET     /projects/:id/demo                -- demo config + live pool stats (session auth)
PUT     /projects/:id/demo                -- enable/disable/update config
POST    /projects/:id/demo/reset          -- Mode A: reset now
GET     /projects/:id/demo/sessions       -- list sessions/instances (started, last seen, expiry, status)
DELETE  /projects/:id/demo/sessions/:sid  -- kill session + teardown instance
POST    /projects/:id/demo/baseline       -- capture current base state as the demo baseline
DELETE  /projects/:id/demo/baseline       -- remove baseline (revert to image-only)
-- public, on the demo host only:
GET     /__demo__/session.json            -- cookie-gated session info for the bar
GET     /__demo__/bar.js, bar.css         -- bar assets
```

Session creation is rate-limited per IP on the demo host (fixed-window, `rate_limit.rs` pattern, e.g. 5/min) to blunt scripted instance farming.

---

## Dashboard Changes

### Placement — Settings → General tab

`SettingsPopover.tsx` General tab gets a **"Demo mode" toggle** (web projects only — hidden when `is_background`, same gating as Routes). Toggling on creates the `demo_config` row with defaults; the mode and all settings are then edited from the demo popover on the card.

### Card — demo replaces the lifecycle chips

When demo is enabled, the chip row at `ProjectCard/index.tsx` (`Web app / HTTP API` · `Auto-stop · Xm` · `Auto-start`) is **replaced** by a demo section:

- Mode A chip: `Demo · Shared · resets in Xh` (+ "Reset now" action in the popover)
- Mode B chip: `Demo · Session · N/M live` (+ pool state: prewarming/spawning)
- A settings icon beside it opens a **DemoSettingsPopover** (same pattern as `SleepPopover`) with: mode selector, TTL, max instances, prewarm, reset interval, banner text editor, bar toggle, demo-data section (capture/remove baseline, captured-at + size, re-capture hint after image deploys), disable-demo action

The auto-stop/auto-start chips and the Sleep button are hidden while demo is on — demo owns lifecycle. Auto-stop remains active underneath (instances sleep when idle, waker cold-starts on return); it's just no longer operator-configurable.

### Mode B — instances under the base project

An instances expander on the card (under the chips): per instance — status (running/sleeping/expired), started at, last activity, expiry countdown, kill button. Instance subdomains are shown for operator debugging only (they redirect for visitors).

---

## Edge Cases & Security

- **Parent deleted** → cascade: sessions die with parent (FK), janitor reconciles orphaned instances
- **Capabilities inherited**: clones get parent's `HostNetwork`/`RawPorts`/`DockerObserve` if enabled — operator's choice; document in the enable dialog
- **Resource caps inherited**: each instance capped like the parent (default 256 MB / 0.5 CPU)
- **Cross-node**: any node — the demo host routes through the master (see "Cross-node"); agents need zero demo logic
- **Redeploy while demo is on**: live sessions keep running their image; new clones (incl. prewarm refills) pick up the new spec/image. No rolling restart of live sessions. A captured baseline is data-only — the popover hints to re-capture after image changes.
- **Baseline is per-parent**: one seed per demo parent, stored under `data/demo-seeds/{id}`; shown with its size; deleted with the project.
- **Orchestrator restart mid-session**: sessions + configs in SQLite, gatekeeper resumes, TTL enforcement continues
- **Instance ids** consume real subdomains (`{id}.{domain}`) — hidden from the grid, routed via the gatekeeper only, no DNS records; `ids.rs` allocation already avoids collisions
- **Absolute-URL apps**: `Host` rewriting keeps visitor-visible URLs correct; apps reading `X-Forwarded-Host` see instance identity

---

## Implementation Order

1. Migration `0036_demo_mode.sql`: `demo_parent_id`, `demo_config`, `demo_sessions`
2. Copy engine: extract clone internals (empty-volume + baseline-restore paths, shared images) + baseline capture to `data/demo-seeds/`
3. Reset engine (Mode A core; restores from baseline when set) + janitor hook + "resetting" page
4. Dashboard: General-tab toggle, demo chip + `DemoSettingsPopover` (Mode A fields, Reset now) → **Mode A shippable here**
5. Gatekeeper: demo host interception + route sync (incl. `cloudflare_dns` re-point to master), session issue/lookup, streaming proxy + agent-node upstream hop
6. Mode B pool: atomic slot claim, clone-on-arrival + prewarm refill, pending handshake (`/__demo__/status`), teardown sweep, themed fringe pages (pending/wait/expired/error)
7. Demo bar injection + session.json
8. Dashboard: Mode B popover fields + instances expander → **Mode B shippable**
9. dev-s1 e2e (shared reset cycle, session lifecycle, capacity, restart recovery, agent-node demo)

Related: `access-control.md` (same caddy route surface; password protection could later gate demos), `template-catalog.md` (the baseline mechanism doubles as template seeding if a catalog ever lands).

## Known gaps / follow-ups (2026-11-08)

1. **Deleting a demo parent FK-errors** — `demo_parent_id` has no cascade, so `DELETE /projects/{id}` fails until children are deleted. Fix: `delete_project` tears down sessions + edit/shared + pool instances before deleting the parent.
2. **Parent card stats ignore instances** — CPU/mem/disk tiles reflect only the (stopped) parent; they should sum all demo children, and the instances modal can show per-instance rows.
3. **Mode switch leaves the shared instance running** — switching shared → session should stop it (keep it for an instant switch-back).
4. **Parent redeploy doesn't refresh edit/shared instances** — they keep the old image and `.env` (pool spawns pick the new spec up anyway). Fix: recreate edit/shared on redeploy/recreate completion. Same applies to parent limit and env changes — they propagate to new instances only.
5. **Migrating a demo parent** leaves instances on the old node — block or move them with it.
6. **Agent-node demo is code-complete but not e2e-tested** (mTLS proxy hop, cloudflare re-point) — dev-s1.
7. **Custom demo domains** in zones litebin doesn't control need a manual re-point to the master.
8. **Bar injection is best-effort** — SPAs that rewrite `<body>` can lose it on client-side navigation; pages over 2 MiB pass through unbarred.
9. **Duplicating/exporting a demo parent** yields a plain project — demo config and baseline seeds don't travel (acceptable, but surprising if unexpected).
10. **No node-level demo budget** — N demo projects × `max_instances` each; operator discipline only (global cap explicitly rejected during design).
