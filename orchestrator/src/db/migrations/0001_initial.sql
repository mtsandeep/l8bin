CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    email TEXT,
    is_admin BOOLEAN NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL DEFAULT 'system',
    image TEXT NOT NULL,
    internal_port INTEGER NOT NULL,
    mapped_port INTEGER,
    container_id TEXT,
    status TEXT NOT NULL DEFAULT 'stopped',
    last_active_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE TABLE IF NOT EXISTS deploy_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    name TEXT,
    last_used_at INTEGER,
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tower_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL,
    expiry_date INTEGER NOT NULL
);
