-- Token access levels: cumulative ladder read < deploy < manage < admin.
-- Existing tokens keep their deploy/upload behavior (deploy also grants read).
ALTER TABLE deploy_tokens ADD COLUMN scope TEXT NOT NULL DEFAULT 'deploy';
