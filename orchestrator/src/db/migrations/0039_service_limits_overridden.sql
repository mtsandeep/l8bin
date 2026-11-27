-- Track whether a compose service's memory/CPU limits were explicitly overridden
-- via the dashboard (project card or service settings) so compose redeploys
-- preserve dashboard overrides while still letting compose.yaml win initially.
ALTER TABLE project_services ADD COLUMN limits_overridden INTEGER NOT NULL DEFAULT 0;
