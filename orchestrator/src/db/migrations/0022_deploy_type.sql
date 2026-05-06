ALTER TABLE projects ADD COLUMN deploy_type TEXT NOT NULL DEFAULT 'image';

-- Backfill: multi-service projects are always compose
UPDATE projects SET deploy_type = 'compose' WHERE service_count > 1;
