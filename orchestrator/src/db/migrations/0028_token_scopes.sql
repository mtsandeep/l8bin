-- Token access levels: cumulative read < deploy < manage < admin.
-- Existing tokens become 'deploy' (which also grants read).
ALTER TABLE deploy_tokens ADD COLUMN scope TEXT NOT NULL DEFAULT 'deploy';
