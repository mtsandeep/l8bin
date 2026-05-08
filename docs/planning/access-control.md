# Access Control: Maintenance Mode + Password Protection

Per-project access control with two modes. Both share the same password backend — the only difference is which HTML template Caddy serves.

---

## Two Modes

| | Maintenance Mode | Password Protected |
|---|---|---|
| **Visitor sees** | 503 "Under Maintenance" page | Password entry page |
| **App state** | Containers running but unreachable | Containers serve normally to authed users |
| **Use case** | During migration, planned downtime | Staging sites, client demos, private apps |

Toggle between them with a single `access_mode` flag on the project. Both use identical Caddy cookie-bypass mechanism.

---

## User Experience

```
1. Dashboard → Project Settings → "Access Control"
2. Choose mode: Maintenance / Password Protected / None
3. Admin password auto-generated on first enable (regenerate anytime, cannot delete)
4. Create disposable passwords as needed (custom or auto-generated, default 1-day expiry)
5. Share passwords with anyone who needs access
6. Delete disposable passwords to revoke access immediately
```

---

## Password System

Two groups of passwords, same underlying mechanism:

### Admin Password (1 per project)

- Auto-generated on first enable, shown in dashboard
- Can be regenerated (swaps both password and token)
- Cannot be deleted — always present when access control is enabled
- Long-lived, no short expiry pressure

### Disposable Passwords (0..N per project)

- Same as admin but with more options
- Custom password (e.g., "john2024") or auto-generated
- Default 1-day expiry (configurable per token)
- Can be deleted anytime → immediate session invalidation
- Can be regenerated (keeps same row, swaps password + token)
- Expired tokens remain visible in dashboard (greyed out) until user manually deletes them
- Expired tokens excluded from Caddy config on next route sync

---

## Password vs Token Separation

Passwords and tokens are separate for security:

- **Password**: what the visitor types (human-friendly, shareable). Sent once over HTTPS during verification.
- **Token**: random chars generated internally (e.g., `a7x9k2m`). Stored in cookie + Caddy config. Never exposed as password.

```
Visitor enters password "john2024"
  → POST /access/:id/verify
  → API verifies password, returns token "a7x9k2m"
  → JS sets cookie: l8b_access=a7x9k2m
  → Caddy matches cookie → proxies to app
```

Someone inspecting browser cookies sees the token, not the password. Deleting a token from DB + route sync invalidates access immediately.

---

## How It Works

### Caddy Config

For a restricted project with tokens `a7x9k2m` (admin) and `p3q8r1` (disposable):

```json
{
    "match": [{
        "host": ["myapp.l8b.in"],
        "not": [
            { "cookie": { "l8b_access": "a7x9k2m" } },
            { "cookie": { "l8b_access": "p3q8r1" } }
        ]
    }],
    "handle": [{
        "handler": "static_response",
        "status_code": 503,
        "body": "<maintenance page or password entry page HTML>"
    }]
}
```

Multiple `not` entries are OR'd by Caddy — cookie matching any valid token falls through to the normal proxy route. Route sync rebuilds Caddy config whenever passwords are created, deleted, or regenerated.

For **master_proxy** mode: the master Caddy serves the page directly — no traffic reaches the agent.
For **cloudflare_dns** mode: the agent's Caddy directly returns the page.

### Maintenance Mode Details

When maintenance mode is active:

- Source containers remain running but are unreachable through Caddy — no new data is written
- DNS-cache stragglers hitting the old server see the maintenance page instead of the live app
- No data divergence between source and target — the user can switch DNS at any time

**Why not just stop the source containers?** Stopping containers would cause the waker to try auto-starting them (if `auto_start_enabled`). Maintenance mode at the Caddy level avoids this — containers stay running, waker is bypassed, and the response is immediate (no wake delay).

### Password Entry Page

When password-protected mode is active:

- The static_response page includes a password input field and submit button
- JS POSTs the password to `POST /access/:project_id/verify` (orchestrator API)
- On success, the API returns the token, JS sets the cookie and reloads
- On failure, the page shows an error message (no reload)

### Immediate Session Invalidation

When a disposable password is deleted:

1. Token removed from DB
2. Route sync triggered → Caddy config rebuilt without that token
3. Next request with old cookie → Caddy doesn't match → access page shown
4. Effective immediately (Caddy config reload < 1s)

---

## Data Model

```sql
-- Per-project access mode flag
ALTER TABLE projects ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'none';
-- 'none' | 'maintenance' | 'password_protected'

-- Passwords/tokens table (admin + disposable in one table)
CREATE TABLE project_access_tokens (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL DEFAULT 'disposable',  -- 'admin' | 'disposable'
    password    TEXT NOT NULL,          -- what visitor types
    token       TEXT NOT NULL UNIQUE,   -- random chars, used in cookie + Caddy
    label       TEXT,                   -- optional label (e.g., "For John")
    expires_at  INTEGER,                -- NULL = long-lived (admin), or set expiry
    created_at  INTEGER NOT NULL
);
```

---

## API Endpoints

```
POST   /projects/:id/access                     -- Set access mode (none/maintenance/password_protected)
PUT    /projects/:id/access/admin               -- Regenerate admin password + token
POST   /projects/:id/access/tokens               -- Create disposable password
GET    /projects/:id/access/tokens               -- List all passwords (admin + disposable)
PUT    /projects/:id/access/tokens/:id           -- Regenerate disposable password + token
DELETE /projects/:id/access/tokens/:id           -- Delete disposable (immediate logout)
POST   /access/:project_id/verify                -- Verify password, return token (public, no auth)
```

### Verify Endpoint Detail

`POST /access/:project_id/verify` is **public** (no dashboard auth required) — visitors need to hit it from the password entry page.

**Request:**
```json
{ "password": "john2024" }
```

**Response (success):**
```json
{ "token": "a7x9k2m" }
```

**Response (failure):**
```json
{ "error": "invalid_password" }, 401
```

The JS on the password page sets the cookie from the response and reloads. Failed attempts show an inline error — no page reload.

---

## Dashboard Changes

- Project settings: access mode selector (None / Maintenance / Password Protected)
- Admin password section: show password (copyable), regenerate button
- Disposable passwords section: list with label, password (copyable), expiry, status (active/expired), regenerate/delete buttons
- "Create Password" form: optional label, custom password or auto-generate, expiry (default 1 day)

---

## Relationship to Migration (Phase 2)

Maintenance mode was originally designed as part of the migration flow (Phase 2). With access control as a standalone feature:

- Maintenance mode is now a **general-purpose feature** available on any project, not just during migration
- The migration flow can still enable maintenance mode automatically (`access_mode: 'maintenance'` in the migrate request), but it's no longer tightly coupled
- The `migrated` flag and migration-specific logic remain in Phase 2
- Phase 2 migration plan references this doc for maintenance mode implementation details

---

## Implementation Order

1. DB migration: `access_mode` column + `project_access_tokens` table
2. `ProjectAccessToken` model
3. Access control API endpoints (`/projects/:id/access/*`, `/access/:id/verify`)
4. `maintenance_page_html()` + `password_entry_page_html()` in `waker_pages.rs`
5. Caddy routing changes: `static_response` with cookie bypass in `routing.rs` + `cloudflare_router.rs`
6. Route resolution: include tokens in `routing_helpers.rs`, trigger sync on password changes
7. Dashboard UI: access control section in project settings
