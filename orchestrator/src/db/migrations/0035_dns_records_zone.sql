-- Zone a ledger record lives in; '' means the currently configured zone
-- (rows predating zone-aware sync all do).
ALTER TABLE dns_records ADD COLUMN zone_id TEXT NOT NULL DEFAULT '';
