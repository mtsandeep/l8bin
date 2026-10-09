-- GitHub integration: one instance-level connection (GitHub App or PAT),
-- installations visible to that connection, and per-project repo links that
-- drive generated GitHub Actions workflows.
CREATE TABLE IF NOT EXISTS github_connection (
    id INTEGER PRIMARY KEY CHECK (id = 1),          -- singleton row
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
    status TEXT NOT NULL DEFAULT 'active',          -- active | degraded | disconnected
    status_message TEXT,
    created_by TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS github_installations (
    id INTEGER PRIMARY KEY,                         -- GitHub installation id
    account_login TEXT NOT NULL,
    account_type TEXT NOT NULL DEFAULT 'User',
    account_avatar TEXT,
    repository_selection TEXT NOT NULL DEFAULT 'selected',  -- selected | all
    status TEXT NOT NULL DEFAULT 'active',          -- active | suspended | deleted
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS project_github_config (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    repo_full_name TEXT NOT NULL,                   -- "owner/repo"
    repo_id INTEGER,                                -- survives renames
    branch TEXT NOT NULL DEFAULT '',                -- '' = repo default branch
    workflow_path TEXT NOT NULL DEFAULT '.github/workflows/litebin-deploy.yml',
    deploy_kind TEXT NOT NULL DEFAULT 'single',     -- single | compose
    compose_file TEXT,
    -- JSON of WorkflowInputs (port/cmd/dockerfile/memory/cpu/node_id/compose_services)
    -- so config edits re-render the workflow without re-parsing it from the repo.
    workflow_inputs TEXT,
    auto_deploy INTEGER NOT NULL DEFAULT 0,
    deploy_token_id TEXT REFERENCES deploy_tokens(id) ON DELETE SET NULL,
    installation_id INTEGER REFERENCES github_installations(id) ON DELETE SET NULL,  -- NULL in PAT mode
    -- Denormalized last-run snapshot, updated opportunistically when polled
    last_run_id INTEGER,
    last_run_status TEXT,
    last_run_conclusion TEXT,
    last_run_url TEXT,
    last_run_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
