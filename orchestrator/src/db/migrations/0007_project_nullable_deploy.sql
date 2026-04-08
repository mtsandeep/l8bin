-- Allow creating projects without deployment data (image/port)
-- so users can create a project and generate a deploy token before first deploy.
-- Also recreate deploy_tokens to fix FK that points to projects_old (broken by this migration).

-- Recreate projects table (makes image/internal_port nullable)
ALTER TABLE projects RENAME TO projects_old;

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL DEFAULT 'system',
    image TEXT,
    internal_port INTEGER,
    mapped_port INTEGER,
    container_id TEXT,
    status TEXT NOT NULL DEFAULT 'stopped',
    last_active_at INTEGER,
    node_id TEXT REFERENCES nodes(id),
    auto_stop_enabled INTEGER NOT NULL DEFAULT 1,
    auto_stop_timeout_mins INTEGER NOT NULL DEFAULT 15,
    auto_start_enabled INTEGER NOT NULL DEFAULT 1,
    cmd TEXT,
    memory_limit_mb INTEGER,
    cpu_limit REAL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id)
);

INSERT INTO projects (id, user_id, image, internal_port, mapped_port, container_id, status, last_active_at, node_id, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, created_at, updated_at)
SELECT id, user_id, image, internal_port, mapped_port, container_id, status, last_active_at, node_id, auto_stop_enabled, auto_stop_timeout_mins, auto_start_enabled, cmd, memory_limit_mb, cpu_limit, created_at, updated_at FROM projects_old;

DROP TABLE projects_old;

-- Recreate deploy_tokens to fix FK (now points to projects_old due to the rename above)
ALTER TABLE deploy_tokens RENAME TO deploy_tokens_old2;

CREATE TABLE deploy_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    project_id TEXT,
    token_hash TEXT NOT NULL,
    name TEXT,
    last_used_at INTEGER,
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

INSERT INTO deploy_tokens (id, user_id, project_id, token_hash, name, last_used_at, expires_at, created_at)
SELECT id, user_id, project_id, token_hash, name, last_used_at, expires_at, created_at FROM deploy_tokens_old2;

DROP TABLE deploy_tokens_old2;
