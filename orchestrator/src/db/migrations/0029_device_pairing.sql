-- Device pairing: `l8b login` obtains a scoped token via a short-lived code
-- approved from an authenticated dashboard session.
CREATE TABLE device_codes (
    id TEXT PRIMARY KEY,
    user_code TEXT NOT NULL UNIQUE,     -- human-typed verifier, e.g. L8B-4KX2QW
    client_name TEXT,                   -- shown on the approve page
    suggested_scope TEXT NOT NULL DEFAULT 'deploy',
    scope TEXT,                         -- final scope, set at approval
    project_id TEXT,                    -- optional project binding, set at approval
    user_id TEXT,                       -- approver, set at approval
    status TEXT NOT NULL DEFAULT 'pending',  -- pending | approved | denied | claimed
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
