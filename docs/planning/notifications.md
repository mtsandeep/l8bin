# Notifications

LiteBin pushes events to an external notification router. LiteBin does not handle channel routing, provider configuration, or delivery timing — that's the router's job.

## Philosophy

LiteBin generates events (deploy failed, crash loop, agent offline). An external tool receives them and delivers to the user's preferred channels (Discord, Telegram, email, SMS, etc.). LiteBin only writes to a local outbox and POSTs JSON. Zero provider code, zero channel config.

**Why external router instead of built-in:**
- Provider APIs change constantly (Telegram bot API v2, Discord webhook format, WhatsApp Business API)
- Each provider has its own auth model (tokens, API keys, OAuth)
- Users want different routing rules per project/tag (prod errors → SMS, staging → ntfy)
- Building and maintaining 20+ provider integrations is a full-time job
- External tools like Apprise already solve this

**Recommended routers:**
- [Apprise](https://github.com/caronc/apprise) — 100+ channels, self-hosted API, Docker
- Custom notification router — Tauri desktop widget, React Native mobile app, or web service

---

## Architecture

```
LiteBin                              Notification Router
┌──────────────────┐                 ┌──────────────────────────┐
│ orchestrator     │   HTTP POST     │                          │
│  ├ deploy fail ──┼──┐              │  Apprise / custom tool   │
│  ├ auto-update ──┼──┤              │  ├→ Discord              │
│  ├ crash loop ───┼──┤  outbox      │  ├→ Telegram             │
│  └ agent down ───┼──┼─────────────►│  ├→ Email (SMTP)         │
│                    │  dedupe       │  ├→ ntfy                 │
│ notification      │  immediate     │  └→ SMS (Twilio)         │
│ _outbox table     │                │                          │
└──────────────────┘                 │  Routing rules:          │
                                     │  tags=prod + error       │
                                     │    → Discord + SMS       │
                                     │  tags=staging            │
                                     │    → ntfy only           │
                                     │  quiet hours 11PM-7AM    │
                                     │    → batch digest        │
                                     └──────────────────────────┘
```

LiteBin's responsibilities:
1. Write event to outbox table
2. Dedupe within window (same event + project = merged)
3. POST to configured endpoint
4. Retry on failure (with backoff)
5. Include tags and metadata for router to filter

Router's responsibilities:
1. Receive JSON payload
2. Match routing rules (tags, severity, event_type)
3. Deliver to configured channels
4. Handle quiet hours, grouping, rate limiting
5. Manage provider auth (tokens, API keys)

---

## DB Schema

### `notification_config` (global, singleton)

```sql
CREATE TABLE notification_config (
  id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0,
  endpoint_url TEXT NOT NULL,              -- e.g., "http://192.168.1.5:8090/notify"
  endpoint_token TEXT,                     -- optional Bearer token
  dedupe_window_seconds INTEGER NOT NULL DEFAULT 300,  -- 5 min
  retention_days INTEGER NOT NULL DEFAULT 7,
  severity_filter TEXT NOT NULL DEFAULT 'warning,error',  -- comma-separated
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

| Field | Purpose |
|-------|---------|
| `enabled` | Master toggle |
| `endpoint_url` | Where to POST notifications |
| `endpoint_token` | Optional auth for the endpoint |
| `dedupe_window_seconds` | Same event_type + project_id within this window = merged into one notification |
| `retention_days` | How long to keep sent/failed entries in outbox before cleanup |
| `severity_filter` | Which severities to forward. Events not matching this filter are not written to outbox at all |

### `notification_outbox`

```sql
CREATE TABLE notification_outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_type TEXT NOT NULL,               -- "deploy_failure", "crash_loop", "agent_offline"
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  severity TEXT NOT NULL DEFAULT 'info',  -- "info", "warning", "error"
  project_id TEXT,
  project_name TEXT,
  node_id TEXT,
  node_name TEXT,
  tags TEXT DEFAULT '[]',                 -- JSON array, resolved from project at send time
  metadata TEXT,                          -- JSON: extra context
  status TEXT NOT NULL DEFAULT 'pending', -- "pending", "sent", "failed"
  retry_count INTEGER NOT NULL DEFAULT 0,
  next_retry_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  sent_at TEXT
);
```

### Project tags

```sql
ALTER TABLE projects ADD COLUMN tags TEXT DEFAULT '[]';  -- JSON array: ["prod", "api", "team-frontend"]
```

Tags are resolved from the project at send time and included in the notification payload. The router uses them for routing rules. LiteBin does not filter by tags.

---

## Event Sources

### Orchestrator events

| Source | Event Type | Severity | When |
|--------|-----------|----------|------|
| Deploy | `deploy_success` | info | Container healthy after deploy |
| Deploy | `deploy_failure` | error | Deploy fails or health check timeout |
| Liveness probe | `crash_loop` | error | Container hit max restarts, marked unhealthy |
| Liveness probe | `unhealthy_recovered` | info | Previously unhealthy project is healthy again |
| Auto-update | `update_available` | warning | New image detected for a running project |
| Auto-update | `warm_start_failed` | error | Warm start after image update failed |
| Backup (platform) | `backup_failed` | error | Litestream sync failure |
| Backup (project) | `project_backup_failed` | error | Rustic backup failure |
| Agent health | `agent_offline` | error | Agent heartbeat missed |
| Agent health | `agent_reconnected` | info | Agent back online after being offline |

### Agent events

Agents POST to the orchestrator's internal endpoint for events they detect locally:

| Source | Event Type | Severity | When |
|--------|-----------|----------|------|
| Agent | `agent_disk_warning` | warning | Disk usage > 85% |
| Agent | `agent_disk_critical` | error | Disk usage > 95% |
| Agent | `agent_docker_error` | error | Docker daemon unreachable |

```
Agent → POST /internal/notifications { event_type, title, body, severity, node_id }
Orchestrator → writes to outbox → flushes to router
```

---

## Delivery Flow

### Send

```rust
pub async fn send(
    event_type: &str,
    title: &str,
    body: &str,
    severity: &str,
    project_id: Option<&str>,
    node_id: Option<&str>,
    metadata: Option<Value>,
) {
    // Skip if notification not enabled or severity filtered out
    if !config.enabled || !config.severity_filter.contains(severity) {
        return;
    }

    // Resolve tags from project
    let tags = project_id
        .and_then(|id| db::get_project_tags(id).await.ok())
        .unwrap_or_default();

    // Write to outbox
    db::insert_notification(event_type, title, body, severity, project_id, tags, metadata).await;

    // Flush immediately — all events go out right away
    // Dedupe handles grouping, router handles pacing
    flush_pending().await;
}
```

### Flush (immediate, always)

Every `send()` triggers a flush. No timer, no batch interval. Dedupe is the only mechanism that prevents spam.

```
flush_pending():
  1. SELECT * FROM notification_outbox WHERE status = 'pending'
  2. Dedupe: group by (event_type, project_id) within dedupe_window
     - 10 crash_loop events for "myapp" in 2 seconds → 1 row: "crash_loop (10 occurrences)"
  3. POST to endpoint_url as JSON:
     {
       "notifications": [...],
       "server": "my-litebin",
       "sent_at": "2026-05-06T10:30:00Z"
     }
  4. On success: mark all as "sent", set sent_at
  5. On failure: increment retry_count, set next_retry_at with backoff
```

### Retry with backoff

```
Retry 1: immediately (next flush cycle)
Retry 2: after 5 seconds
Retry 3: after 30 seconds
Retry 4: after 120 seconds
Retry 5+: mark "failed", stop retrying
```

```sql
-- Exponential backoff
-- next_retry_at = now + (5 * 2^(retry_count-1)) seconds
-- retry_count=1 → 5s, retry_count=2 → 10s, retry_count=3 → 20s, retry_count=4 → 40s
```

### Cleanup

Background task runs daily (same tokio-cron-scheduler as backup):

```sql
DELETE FROM notification_outbox
WHERE created_at < datetime('now', '-' || retention_days || ' days');
```

---

## Payload Format

What LiteBin POSTs to the router:

```json
{
  "notifications": [
    {
      "event_type": "crash_loop",
      "title": "Crash loop detected: myapp",
      "body": "Container restarted 3 times, marked unhealthy",
      "severity": "error",
      "project_id": "myapp",
      "project_name": "My App",
      "tags": ["prod", "api"],
      "node_id": "agent-1",
      "node_name": "US-East",
      "occurrences": 3,
      "metadata": {
        "restart_count": 3,
        "max_restarts": 3,
        "image": "ghcr.io/me/myapp:latest"
      }
    },
    {
      "event_type": "deploy_success",
      "title": "Deployed: blog",
      "body": "Deployed ghcr.io/me/blog:abc123 in 12s",
      "severity": "info",
      "project_id": "blog",
      "project_name": "Blog",
      "tags": ["personal"],
      "node_id": "local",
      "node_name": "Local",
      "occurrences": 1,
      "metadata": {
        "image": "ghcr.io/me/blog:abc123",
        "deploy_duration_seconds": 12
      }
    }
  ],
  "server": "my-litebin",
  "sent_at": "2026-05-06T10:30:00Z"
}
```

### Router contract

The router must:
1. Accept POST with JSON body matching the format above
2. Return `200 OK` on success (any body is fine)
3. Return `429` or `5xx` on failure (LiteBin will retry with backoff)

That's the entire contract. LiteBin doesn't care what the router does with the payload.

---

## Dashboard

### Settings

```
Notifications
─────────────────────────────────────────────────────
Enabled:     [toggle]
Endpoint:    http://192.168.1.5:8090/notify
Token:       ********  [show] [test]
Dedupe:      5 min
Retention:   7 days
Severity:    [x] warning  [x] error  [ ] info
─────────────────────────────────────────────────────
[Test Notification]  [Save]
```

### Notification Log (dedicated dashboard page)

Separate "Notifications" page in the dashboard nav (not mixed into settings).

```
Notifications
─────────────────────────────────────────────────────
crash_loop      myapp      error   2 min ago   ✓ sent
deploy_success  blog       info    5 min ago   ✓ sent
agent_offline   US-East    error   1 hour ago  ✓ sent
deploy_failure  api        error   2 hours ago ✗ failed (4 retries)
deploy_success  myapp      info    2 hours ago ✓ sent
─────────────────────────────────────────────────────
Filters: [severity ▾] [project ▾] [event type ▾]
Showing last 50 · [Clear failed]
```

**Dashboard nav:**

```
Projects | Nodes | Notifications | Settings
```

Settings page only has notification config (endpoint, token, dedupe, severity filter, test button). The Notifications page is the log with filters — where you go to investigate.

**Works without a router configured.** Events matching the severity filter are always written to the outbox. If no endpoint is set, the flush skips the HTTP POST but events are still recorded. The page shows what happened on the server regardless. A banner links to settings when no endpoint is configured.

```
Notification Log
─────────────────────────────────────────────────────
crash_loop      myapp      error   2 min ago   ✓ sent
deploy_success  blog       info    5 min ago   ✓ sent
agent_offline   US-East    error   1 hour ago  ✓ sent
deploy_failure  api        error   2 hours ago ✗ failed (4 retries)
deploy_success  myapp      info    2 hours ago ✓ sent
─────────────────────────────────────────────────────
Showing last 50 · [Clear failed]
```

---

## CLI

```bash
l8b notify test              -- Send test notification to endpoint
l8b notify log               -- Show recent notification log
l8b notify config            -- Show current config
l8b notify config --url ...  -- Update endpoint URL
```

---

## API Endpoints

```
GET  /admin/notifications/config       -- Get notification config
POST /admin/notifications/config       -- Update notification config
POST /admin/notifications/test        -- Send test notification
GET  /admin/notifications/log          -- Recent notification log (sent/failed), supports ?since= for polling
DELETE /admin/notifications/log        -- Clear failed notifications
POST /internal/notifications           -- Agent → orchestrator: submit notification event
```

---

## Resource Impact

| Component | RAM |
|-----------|-----|
| Outbox table | 0 (part of existing SQLite) |
| Flush on send | ~0 (uses existing reqwest, triggered by event) |
| Cleanup task | ~0 (reuses tokio-cron-scheduler) |
| **LiteBin total increase** | **~0** |
| External router (Apprise) | ~20-30 MB (optional, separate container) |
| External router (custom) | Whatever the tool needs |

Zero RAM overhead on LiteBin. No extra container. No extra dependency.

---

## What This Doesn't Do

| Doesn't | Why |
|---------|-----|
| Route to specific channels | Router's job. LiteBin sends JSON, router decides delivery. |
| Manage provider credentials | Router's job. LiteBin never stores Discord tokens, SMTP passwords, etc. |
| Handle quiet hours | Router's job. LiteBin always sends immediately. |
| Rate limit to providers | Router's job. LiteBin dedupes at the source, router handles downstream pacing. |
| Render rich messages (Markdown, embeds) | Router can transform the payload. LiteBin sends plain text title + body. |
| Retry forever | Max 4 retries with backoff, then mark failed. User investigates in dashboard. |

---

## Implementation Order

| # | Task | Depends On | Complexity |
|---|------|-----------|------------|
| 1 | DB migration: `notification_config`, `notification_outbox`, project `tags` column | — | Low |
| 2 | `notification::send()` function — write to outbox, resolve tags, flush | 1 | Low |
| 3 | Flush logic — dedupe, batch, POST, retry with backoff | 2 | Medium |
| 4 | Agent notification endpoint — `POST /internal/notifications` | 2 | Low |
| 5 | Wire up event sources (deploy, liveness, agent health, backup, auto-update) | 2, 3 | Low |
| 6 | Cleanup task — daily purge of old outbox entries | 1 | Low |
| 7 | Dashboard — config form, test button, notification log, poll-based bell icon | 1-6 | Low |
| 8 | CLI — `l8b notify test/log/config` | 1 | Low |
