ALTER TABLE projects ADD COLUMN demo_parent_id TEXT REFERENCES projects(id);

CREATE INDEX IF NOT EXISTS idx_projects_demo_parent ON projects(demo_parent_id);

-- mode: 'shared' = one instance for all visitors (interval reset);
-- 'session' = clone per visitor (hard TTL)
CREATE TABLE IF NOT EXISTS demo_config (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL DEFAULT 'shared' CHECK (mode IN ('shared', 'session')),
    max_instances INTEGER NOT NULL DEFAULT 5,
    session_ttl_minutes INTEGER NOT NULL DEFAULT 15,
    prewarm INTEGER NOT NULL DEFAULT 1,
    reset_interval_minutes INTEGER,
    banner_text TEXT NOT NULL DEFAULT 'Demo environment — data resets periodically',
    bar_enabled INTEGER NOT NULL DEFAULT 1,
    last_reset_at INTEGER,
    baseline_captured_at INTEGER,
    updated_at INTEGER NOT NULL
);

-- expires_at is absolute and never extended; expiry teardown deletes the
-- instance project
CREATE TABLE IF NOT EXISTS demo_sessions (
    id TEXT PRIMARY KEY,
    parent_project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    instance_project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'expired', 'ended'))
);

CREATE INDEX IF NOT EXISTS idx_demo_sessions_parent ON demo_sessions(parent_project_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_demo_sessions_token ON demo_sessions(token_hash);

-- seed volumes live on the parent's node; the project-dir seed tar sits in
-- data/demo-seeds/{id}/
CREATE TABLE IF NOT EXISTS demo_baseline_volumes (
    parent_project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_volume TEXT NOT NULL,
    seed_volume TEXT NOT NULL,
    PRIMARY KEY (parent_project_id, source_volume)
);
