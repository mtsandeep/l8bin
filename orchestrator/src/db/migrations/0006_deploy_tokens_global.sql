-- Allow deploy tokens to be global (project_id = NULL) or project-scoped
ALTER TABLE deploy_tokens RENAME TO deploy_tokens_old;

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

INSERT INTO deploy_tokens SELECT * FROM deploy_tokens_old;
DROP TABLE deploy_tokens_old;
