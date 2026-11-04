# Platform Domains

How the platform domain works — routing modes, changing it safely, the temporary sslip.io mode, and how LiteBin manages Cloudflare DNS.

---

## Overview

Every project gets a subdomain (`{id}.{domain}`) and the dashboard is served at `{dashboard_subdomain}.{domain}`. Traffic reaches apps in one of two routing modes, and the platform domain can be changed (or dropped entirely for a temporary one) without downtime: the old domain keeps serving for 24h after a change.

## Routing modes

**Master proxy (Local)** — all traffic routes through this server via Caddy reverse proxy. You own DNS: point a wildcard `A *.{domain}` record at the server IP (one record covers the dashboard, poke, and every project subdomain). The DNS target — the IP records should point at — is shown in Settings → Routing & DNS; it auto-detects when unset.

**Cloudflare DNS** — LiteBin creates and maintains the DNS records for you via the Cloudflare API: platform subdomains, project custom domains (including the apex), and re-pointing whenever a node's IP changes. Configure an API token (Zone → DNS → Edit) and the zone ID in Settings → Routing & DNS — *Check credentials* validates them before saving, and *Sync DNS records* reconciles everything on demand. Records already pointing at an IP LiteBin does not know are never touched silently: they are skipped and listed on the button, and *Re-point anyway* confirms the overwrite.

Sync is zone-aware: records for project custom domains that live in other zones of the same Cloudflare account are managed in their own zone. Custom domains hosted outside the Cloudflare account are skipped with a note — their A records are yours to manage manually.

## Changing the platform domain

Settings → Routing & DNS → **Change** opens the wizard:

1. **Domain + routing** — enter the new domain, pick the routing mode (Cloudflare credentials can be entered right here; master proxy shows the `A *.{domain} → {ip}` record to add), or flip on **Temporary mode** to run on the server's auto-assigned sslip.io address.
2. **Check** — preflight validates DNS resolution, the wildcard, Cloudflare credentials, and (in Cloudflare mode) previews what a sync would create/re-point. *Check again* re-runs it as your DNS propagates; the checkbox acknowledges proceeding before DNS is ready.
3. **Apply** — runs as a staged job, visible step by step:
   - **Stage** the new domain in Caddy alongside the old one (nothing persists yet — a failure here changes nothing)
   - **Verify** the new dashboard answers over real DNS (on-demand TLS issues the certificate). With the acknowledgement ticked this is a 15-second sanity check that continues on a miss; otherwise it waits up to 3 minutes and fails if the new URL never answers
   - **Activate**: persist the domain, rebuild routes, re-register agents, and sync DNS in one shot

Nothing is deleted during a change. Conflicting records with a different IP are simply re-pointed by the sync.

### The 24-hour grace window

After a change, the previous domain keeps serving everything (dashboard, project subdomains, poke) for 24h — including requests on the old hostname reaching the orchestrator. The janitor retires it automatically, or the *retire it now* link on the success screen does it immediately. Retiring removes the routes only: **DNS records are never auto-deleted**. Only the latest previous domain is retained; changing again replaces it.

A second domain change while one is running is refused until the job finishes; reopening the wizard re-attaches to a running job (e.g. after a page reload).

### The apex

The bare domain (`example.com` itself) serves a small default landing page bundled with the orchestrator — nothing about the install is exposed (no version, app count, or dashboard link). On sslip.io hosts the title shows the server IP. Claim the apex as a **project custom domain** to serve your own site there instead; removing the claim brings the landing page back. The page lives in the orchestrator source (`orchestrator/src/routes/apex_page.html`) and ships with each release.

## Temporary mode (sslip.io)

Flip **Temporary mode** in the wizard, or run:

```bash
l8b platform domain sslip
```

The server computes `{ip-with-dashes}.sslip.io` from its public IP and switches to master proxy — no domain or DNS needed (sslip.io hostnames resolve globally). Use it when a domain expires or to run LiteBin before deciding on a domain; switch to a real domain later through the same wizard. Let's Encrypt rate limits make sslip domains unsuitable for production traffic.

On the server itself (SSH, no dashboard or CLI login needed):

```bash
curl -fsSL https://l8b.in | sudo bash -s set-domain sslip
```

This is the recovery path: it sets the domain directly and restarts the orchestrator — instant switch, no staged job or grace window.

## Removing Cloudflare

Settings → Routing & DNS offers two removal actions:

- **Remove** — clears credentials and switches to master proxy; DNS records stay in Cloudflare untouched
- **Remove + delete records** — also deletes the LiteBin-managed A records first (records you created yourself are never touched)

## CLI

```bash
l8b platform domain show                                     # domain, routing mode, grace window
l8b platform domain set example.com [--ack-dns]              # change the domain
l8b platform domain set example.com --mode cloudflare-dns \
     --cf-token … --cf-zone …                                # with routing settings
l8b platform domain sslip                                    # temporary sslip.io mode
```

`set` runs the same staged job (preflight → apply → poll) as the dashboard; `--ack-dns` accepts preflight warnings without prompting.
