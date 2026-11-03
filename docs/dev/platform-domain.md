# Platform Domain & Settings

The platform domain is DB-backed and live-editable: Dashboard → Settings runs a staged domain-change job (stage → verify → activate, with the old domain served for a 24h grace window). Tryout installs get a full platform on a free sslip.io domain; production expects a real domain. User-facing behavior: [Platform domains](../topics/domains.md).

## Product stance

- **Production:** real domain (or subdomain as `DOMAIN`). Not an IP:port PaaS.
- **Tryout:** install may set `DOMAIN={public-ip-dashed}.sslip.io` (e.g. `203.0.113.5` → `203-0-113-5.sslip.io`). **Full LiteBin** — dashboard, app deploys, agents. LE may fail; user accepts (banner). No deploy block.
- **Agents on tryout:** yes. Master pushes the same sslip `DOMAIN` via `/internal/register`. Under **master_proxy**, public traffic still hits the master (`{project}.{DOMAIN}` → master IP); agents don't need their own sslip names.
- **Upgrade path:** Settings platform-domain change (staged job; sslip → real domain or back via the temporary-mode toggle / `l8b platform domain sslip`).
- **Master proxy DNS:** `*.{DOMAIN}` → LiteBin; **specific records override wildcards** (`mail.` / `app.` elsewhere OK). Apex ≠ wildcard. Or `DOMAIN=apps.example.com` + `*.apps.example.com`.
- **Cloudflare DNS:** `DOMAIN` may be a subdomain of the CF zone; sync only LiteBin-owned names under the platform DOMAIN suffix.
- **Tryout + Cloudflare:** not supported — tryout forces `master_proxy` (the sslip zone isn't user-controlled).
- **Tryout detection:** domain ending `.sslip.io` / `.nip.io` ⇒ `tryout` flag in `GET /settings` → dashboard banner ("not for production; LE may fail"). Warning-level in preflight; no functional lock.
- **Out of scope:** branded `nip.l8b.in` at scale (LE quota risk on a shared parent domain), bare-IP dashboard (rejected).

## Source of truth

```mermaid
flowchart LR
  install[".env at install"] --> seed["INSERT OR IGNORE settings"]
  seed --> db["settings table"]
  db --> runtime["PlatformSettings in AppState"]
  modal["Settings modal"] --> db
  modal --> runtime
  runtime --> routes["Caddy / CF / register"]
```

| Field | Seed | Runtime | UI |
|---|---|---|---|
| `domain` | `.env` once | DB + in-memory | Heavy change flow |
| `dashboard_subdomain` | `.env` once | DB + in-memory | Simple save + sync |
| `poke_subdomain` | `.env` once | DB + in-memory | No UI |
| `dns_target` | empty / user | DB | IP DNS records point at (fallback: `PUBLIC_IP` env, else detect) |
| `previous_domain` + `retire_at` | change job | DB + in-memory | 24h grace window; janitor retires |
| `staged_domain` | change job (prepare) | DB + in-memory | Served + TLS-approved while the job verifies |
| `routing_mode`, CF token/zone | existing | DB (startup reads DB) | Existing |
| Infra (`PORT`, certs, `DATABASE_URL`, …) | `.env` | env forever | none |

Module: [`orchestrator/src/platform.rs`](../../orchestrator/src/platform.rs) — `PlatformHandle` / `PlatformSettings` / domain job types. Runtime code reads `state.platform.*`; `config.domain` survives only at startup/seed — there are no stale runtime reads.

## Change-flow defaults

- **Preflight:** takes the operator's routing-mode selection and typed Cloudflare credentials (falling back to the saved ones). Cloudflare mode hard-fails if the zone doesn't cover the new domain or the token can't manage records. Master-proxy: one DNS check (wildcard probe, dashboard-record fallback) warns when nothing resolves; the ack checkbox lets the user proceed without verified DNS.
- **Staged job:** `prepare_routes` stages the new domain in Caddy next to the current one (nothing persists; failure is a no-op) → `prepare_verify` polls the new dashboard host (this server's IP only; a foreign resolution fails fast). Acked runs get a 15s sanity window and continue on a miss; unacked runs wait up to 3 minutes and fail. → `persist` records `previous_domain` + 24h `retire_at` and switches → routes/agents/agent-caddy → CF sync (creates + re-points; **never deletes** — conflicting IPs are updated in place).
- **Grace window:** the previous domain keeps serving (Caddy routes + the waker's host gate accepts its hosts) until the janitor retires it or *retire it now* fires. Retirement drops routes only; DNS records are left in Cloudflare. Only the latest previous domain is retained.
- **Removing Cloudflare:** *Remove* clears credentials and switches to master proxy; *Remove + delete records* additionally deletes ledger records first.
- **Poke subdomain:** no Settings UI (PATCH-able via API only).

### Changing the domain without the dashboard

Two paths, for when the dashboard is reachable or not:

| Path | Command | Notes |
|---|---|---|
| CLI (admin token) | `l8b platform domain set <domain> [--ack-dns] [--mode …] [--cf-token …] [--cf-zone …] [--timeout N]`; `l8b platform domain sslip` | Preflight → apply → polls the job; `show` reports domain, routing mode, and the grace window (falls back to read-scoped `/meta` without admin). |
| On-server recovery | `curl -fsSL https://l8b.in \| sudo bash -s set-domain <domain\|sslip>` | Domain-expiry lockout: execs the orchestrator's offline `set-domain` subcommand (`docker exec litebin-orchestrator /app/litebin-orchestrator set-domain <domain>`) and restarts it. `sslip` auto-detects the public IP and builds `<ip-dashed>.sslip.io`. Re-registers online agents best-effort; boot route sync rebuilds Caddy/DNS/agent configs from the DB value. Editing `.env` `DOMAIN` does **not** work — it is an `INSERT OR IGNORE` seed and the DB row wins. |

## DNS record ledger

Every Cloudflare record LiteBin creates is recorded in the `dns_records` table (name → record ID + zone; [`orchestrator/src/dns_ledger.rs`](../../orchestrator/src/dns_ledger.rs)). All deletion decisions — stale-record cleanup during syncs and *Remove + delete records* — are made from this ledger, never by suffix-sweeping the zone: records you created yourself (e.g. `mail.{domain}` at a zone apex) are never touched. Rows carry the zone because sync is zone-aware: custom-domain records live in their own zone and are compared/re-pointed there. A "record does not exist" (81047) response drops the stale row silently.

## API surface

Routes in [`orchestrator/src/routes/global_settings/`](../../orchestrator/src/routes/global_settings) (`mod.rs`, `domain_job.rs`, `dns_ops.rs`), admin scope:

| Method | Path | Role |
|---|---|---|
| `GET` | `/settings` | Live domain + `tryout` flag |
| `PATCH` | `/settings` | Rejects `domain`; dashboard subdomain → sync routes + re-register agents |
| `POST` | `/settings/domain/preflight` | `{ domain, routing_mode?, cf creds? }` → `{ ok, errors[], warnings[], checks[], plan? }` |
| `POST` | `/settings/domain/apply` | `{ domain, acknowledge_dns?, sslip?, routing_mode?, cf creds? }` → `{ job_id }` (409 if a job is pending/running) |
| `GET` | `/settings/domain/jobs/active` | The running/pending job, if any (re-attach after reload) |
| `GET` | `/settings/domain/jobs/{id}` | Poll steps |
| `POST` | `/settings/domain/jobs/{id}/retry` | Resume a failed job from the failed step |
| `GET` | `/settings/domain/sslip` | The server's sslip.io domain (null when the IP is unknown) |
| `POST` | `/settings/domain/retire-previous` | Retire the grace-window domain now |
| `POST` | `/settings/cloudflare/validate` | Validate typed-or-saved CF credentials |

Job steps, in order: `prepare_routes` (stage alongside) → `prepare_verify` (15s sanity when acked / 3-min wait otherwise) → `persist` (domain + grace bookkeeping) → `routes` → `agents` → `agent_caddy` → `dns` (CF sync; `Skipped` in master_proxy) → completed.

## Frontend

[`dashboard/src/components/settings/GeneralPanel.tsx`](../../dashboard/src/components/settings/GeneralPanel.tsx) — domain as text + Change modal (temporary-mode toggle, routing-mode radio with CF credential fields, records checklist, preflight checks with *Check again*, staged job with per-step icons and a progressive reveal into the summary + *retire it now*; re-attaches to running jobs). CF card: *Check credentials* while unsaved, *Sync DNS records* / *Remove* / *Remove + delete records* when saved and clean. Tryout banner. Save never sends `domain`.

## Install tryout

[`install.sh`](../../install.sh) master prompts: detect public IP (ipify → checkip → icanhazip) → "Domain or subdomain ready?" — **Yes** → domain + dashboard/poke + routing + CF prompts; **No** → `DOMAIN={ip-dashed}.sslip.io`, `master_proxy` forced, CF prompts skipped, tryout warnings shown.

## Key call sites (use `state.platform.*`, not `config.domain`)

- [`orchestrator/src/routing_helpers.rs`](../../orchestrator/src/routing_helpers.rs)
- [`orchestrator/src/routes/nodes/connect.rs`](../../orchestrator/src/routes/nodes/connect.rs) (`reregister_online_agents`)
- [`orchestrator/src/nodes/heartbeat.rs`](../../orchestrator/src/nodes/heartbeat.rs)
- deploy ([`routes/deploy/`](../../orchestrator/src/routes/deploy)) / waker / caddy / janitor / reconciliation / activity

## Not built (future)

- **Domain job persistence** — jobs live in an in-memory `DashMap`; a restart loses in-flight job status (the domain value itself stays in DB).
- **Hardening tests** — covered: ask approval for staged/previous hosts, previous-domain route duplication, retirement, sslip preview ([`orchestrator/src/tests/domain_change_tests.rs`](../../orchestrator/src/tests/domain_change_tests.rs)). Still open: apply/retry e2e, sslip → real-domain migration, CF cleanup scoping.
