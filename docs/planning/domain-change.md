> **Status: design finalized 2026-11-02; Phases 1–2 (zone-aware sync, counter split, sync-dns fix, dns_target override, structured preflight + plan preview) SHIPPED 2026-11-02; Phases 3–4 pending**

# Staged Platform Domain Change

## Overview

Replace the flip-first domain change (`/settings/domain/apply` persists the new domain at step 0) with a staged flow: everything is asked and verified up front, the new domain is staged alongside the old one, and activation happens in one shot only after the new dashboard URL is proven reachable. The old domain stays live for a 24h grace window, so a domain change (or a domain expiry) never leaves the operator without a working dashboard.

Motivating incidents (2026-11-02): a sslip.io → real-domain flip killed the old URL before the new `dash.` host had DNS; the Sync DNS button silently no-ops in `master_proxy` mode; custom-domain records live in other Cloudflare zones so the sync's existing-record list never sees them and every sync re-creates them ("An identical record already exists").

## UX

### Change-domain modal — one wizard, three steps

**Step 1 — Domain & routing** (form only, nothing applied):
- New domain input
- *"Temporary / no domain?"* quick action: fills `<public-ip>.sslip.io`, master-proxy mode, no DNS requirements (sslip resolves globally)
- Routing mode radio:
  - **Cloudflare DNS** → extra form: API token + zone id + live **Validate** button
  - **Master proxy** → records checklist with copy buttons: `A dash.<domain> → <ip>`, `A poke.<domain> → <ip>`, `A *.<domain> → <ip>` (wildcard recommended — covers project subdomains)
- Existing Cloudflare settings card is untouched; the modal's form is the only new input surface

**Step 2 — Pre-flight** (hard gate, auto-retry while user fixes DNS):
- `dash.<new>` resolves to this server ✓ (or wildcard probe passes), local 80/443 listening ✓, CF token+zone valid ✓ (CF mode), conflict checks ✓
- Plan preview: records to create / re-point / unchanged
- When `PUBLIC_IP` is unset: orchestrator detects it (outbound request), shows it, and persists it on apply
- sslip domains skip DNS checks (always resolvable) and force master mode

**Step 3 — Apply (auto-activates):**
- **Prepare:** stage new-domain routes in Caddy alongside the old domain; on-demand TLS issues the cert; poll `https://dash.<new>` until the dashboard answers. Old domain works throughout.
- **Activate:** one shot — persist domain + routing/creds, re-register agents, single zone-aware DNS sync (CF mode), record previous domain with 24h retire-at. Auto as soon as prepare is green; no extra click.
- Summary shows the new URL plus *"old domain `<x>` keeps working for 24h"* and a **Retire now** button.

### Old-domain retention

- `previous_domain` + `previous_domain_retire_at` settings; Caddy serves the previous domain's dashboard, project, and poke routes until retired
- Janitor retires automatically after 24h: rebuild Caddy without it, delete its ledger-scoped CF records (creds permitting), clear the settings
- Only the latest previous domain is retained; a second change replaces and retires the first immediately
- No revert button (decided): the grace window covers the transient case; a real revert is just another change

### CLI

- `l8b platform domain sslip` — server computes `<public-ip>.sslip.io`, applies master-proxy mode, skips DNS pre-flight. The domain-expiry lifeline.
- `l8b platform domain set <domain> [--mode master-proxy|cloudflare-dns] [--cf-token …] [--cf-zone …]` — extends the existing command so mode/creds are not dashboard-only
- `l8b platform domain show` — adds routing mode + previous domain / grace remaining

## Backend

### Zone-aware DNS sync (fix, prerequisite)

`sync_dns` currently lists records for the configured zone only, so custom-domain records (a custom domain in its own zone) are invisible: never compared, re-created every sync, `errors=2` forever.

- Group desired names by zone: platform-domain names → configured zone; custom-domain names → zone resolved via the CF zones API (suffix match, cached)
- List + compare + upsert per zone; the IP-compare fix then reaches custom domains
- Treat CF's "An identical record already exists" (81057) as success: locate the record, ledger it, count unchanged
- Split counters: `created` / `repointed` / `unchanged` / `deleted` / `errors` (today a re-point counts as unchanged)

### Sync DNS button (fix)

`/settings/sync-dns` routes through the active router; `MasterProxyRouter::sync_dns_only` is a silent no-op. The endpoint should run the reconcile directly whenever creds are configured, regardless of routing mode; keep the 400 when creds are missing.

### Public IP

- `public_ip` becomes a settings override (env stays the fallback); saved when detected during domain apply; read at router build/hot-swap
- Empty public IP no longer silently skips records — preflight surfaces it

### Domain job — two phases (`domain_job.rs`)

- **Prepare (new, nothing persisted):** build a Caddy config containing both current-domain routes and the new domain's dashboard/poke routes → apply to master Caddy → poll the new dashboard URL (bounded, ~3 min) while on-demand TLS issues the cert. Failure here changes nothing.
- **Activate (existing steps, reordered):** persist domain (+ creds/mode when provided) → rebuild master routes → re-register agents → push agent Caddy → single DNS sync → set `previous_domain`/`retire_at` (old-domain CF record deletion moves to retirement)
- Auto-advance from prepare to activate; job steps report both phases
- Apply request gains `{routing_mode?, cloudflare_api_token?, cloudflare_zone_id?}`; creds persist before preflight when CF mode is selected

### Retirement

- Janitor (or the periodic status sync) checks `retire_at`: rebuild Caddy without the previous domain, delete ledger rows under it via CF (skip with warning if creds are gone), clear settings
- `POST /settings/domain/retire-previous` triggers it early (the *Retire now* button)

### Preflight extensions (`run_domain_preflight`)

- Per-check structured results (not just error strings) for the modal checklist
- Public-IP detection when unset; sslip suffix → skip DNS checks, force master mode
- Wildcard probe (`probe-<rand>.<domain>`) to tell "dash record present" from "wildcard present"
- Plan preview: desired names+IPs vs zone-aware existing state → create/re-point/unchanged counts

## Edge cases

- Back-to-back changes: replacing a previous domain retires the older one immediately (routes + its ledger records)
- CF→master transition: activation sync skipped, but previous-domain retirement still cleans its CF ledger records if creds remain
- master→CF transition: creds from the modal persist before preflight so validation and sync work in the same job
- Same-domain apply: existing hard error
- Let's Encrypt on-demand TLS already applies today for sslip installs; staging adds one cert per change — no new limits
- `unchanged`-vs-`re-pointed` counter split keeps sync logs truthful for support

## Phases

1. **Zone-aware sync + counter split + sync-dns fix** (independent, shippable alone)
2. **Public-IP settings override + preflight extensions** (checks, plan, sslip skip)
3. **Two-phase domain job + previous-domain retention + janitor retirement + retire endpoint**
4. **Modal wizard + CLI (`sslip`, extended `set`, `show`)**

## Verification

Local (repo compose): master-proxy change with missing dash record → preflight blocks with the record checklist; add record → prepare goes green, auto-activate, old domain still answers for 24h (curl both URLs); retire-now removes old routes; CF-mode change with token → one-shot sync re-points (verified via CF API), custom-domain record in a second zone re-pointed, second sync shows `errors=0`; sslip quick action end-to-end; `l8b platform domain sslip` against the local box; back-to-back double change retires the first previous domain.
