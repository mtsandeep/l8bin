-- GitHub connections become multi-row: each GitHub App (or PAT) is its own
-- connection. Private apps can only be installed on their owning account,
-- so covering several GitHub accounts means one app each — and they must be
-- able to coexist. Existing singleton row is carried over as connection 1.
CREATE TABLE github_connection_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    label TEXT,                          -- display: app slug or PAT login
    mode TEXT NOT NULL CHECK (mode IN ('app', 'pat')),
    -- App mode (manifest flow result)
    app_id INTEGER,
    app_slug TEXT,
    client_id TEXT,
    client_secret TEXT,
    private_key_pem TEXT,
    webhook_secret TEXT,
    -- PAT mode (repo + workflow scopes)
    pat_token TEXT,
    pat_login TEXT,
    -- Pending manifest flow (JSON {name, nonce, created_at}) for callback exchange
    pending_manifest TEXT,
    status TEXT NOT NULL DEFAULT 'active',          -- active | pending | degraded | disconnected
    status_message TEXT,
    created_by TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

INSERT INTO github_connection_v2 (
    id, label, mode, app_id, app_slug, client_id, client_secret, private_key_pem,
    webhook_secret, pat_token, pat_login, pending_manifest, status, status_message,
    created_by, created_at, updated_at
)
SELECT
    id,
    CASE WHEN mode = 'pat' THEN pat_login ELSE app_slug END,
    mode, app_id, app_slug, client_id, client_secret, private_key_pem,
    webhook_secret, pat_token, pat_login, pending_manifest, status, status_message,
    created_by, created_at, updated_at
FROM github_connection;

DROP TABLE github_connection;
ALTER TABLE github_connection_v2 RENAME TO github_connection;

-- Installations belong to a specific app connection.
ALTER TABLE github_installations ADD COLUMN connection_id INTEGER REFERENCES github_connection(id) ON DELETE CASCADE;
UPDATE github_installations SET connection_id = 1 WHERE connection_id IS NULL;

-- Links remember which connection serves them (PAT links carry no installation).
ALTER TABLE project_github_config ADD COLUMN connection_id INTEGER REFERENCES github_connection(id) ON DELETE SET NULL;
UPDATE project_github_config SET connection_id = 1 WHERE connection_id IS NULL;
