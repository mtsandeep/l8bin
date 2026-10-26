-- Managed image registries: credentials, mirrors, and the default registry
-- for image names without a registry host. `url` is the canonical Docker auth
-- serveraddress; `auth_b64` is base64 "user:password" (NULL = anonymous);
-- `mirrors` is an ordered JSON array of {"url", "auth_b64"} pull-through
-- endpoints tried before the registry itself.
-- Built-ins are only the registries users plausibly authenticate against
-- (Docker Hub, ghcr.io); everything else pulls anonymously with no row.
CREATE TABLE IF NOT EXISTS registries (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    url         TEXT NOT NULL,
    auth_b64    TEXT,
    mirrors     TEXT,
    is_default  INTEGER NOT NULL DEFAULT 0,
    is_builtin  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Built-in registries; no mirror is seeded.
INSERT OR IGNORE INTO registries (id, name, url, auth_b64, mirrors, is_default, is_builtin, created_at, updated_at) VALUES
    ('docker-hub', 'Docker Hub',                'https://index.docker.io/v1/', NULL, '[]', 1, 1, strftime('%s','now'), strftime('%s','now')),
    ('ghcr',       'GitHub Container Registry', 'https://ghcr.io',             NULL, '[]', 0, 1, strftime('%s','now'), strftime('%s','now'));

-- Agent API protocol version per node, persisted from /health reports so it
-- survives a master restart.
ALTER TABLE nodes ADD COLUMN protocol_version INTEGER NOT NULL DEFAULT 0;
