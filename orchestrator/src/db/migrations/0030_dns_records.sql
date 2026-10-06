-- Ledger of Cloudflare DNS records LiteBin created. Deletion decisions are
-- made from this table (never by suffix-sweeping the zone), so records the
-- user created themselves — e.g. mail.{domain} — are never touched.
CREATE TABLE IF NOT EXISTS dns_records (
    name TEXT PRIMARY KEY,
    record_id TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
