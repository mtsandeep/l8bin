-- Normalized project capability grants (replaces boolean permission columns over time).
CREATE TABLE IF NOT EXISTS project_capabilities (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    granted_at INTEGER NOT NULL,
    granted_by TEXT,
    PRIMARY KEY (project_id, capability)
);

CREATE INDEX IF NOT EXISTS idx_project_capabilities_project
    ON project_capabilities(project_id);

-- Backfill from legacy boolean columns.
INSERT OR IGNORE INTO project_capabilities (project_id, capability, granted_at, granted_by)
SELECT id, 'docker-access', strftime('%s','now'), NULL
FROM projects
WHERE allow_docker_access = 1;

INSERT OR IGNORE INTO project_capabilities (project_id, capability, granted_at, granted_by)
SELECT id, 'raw-ports', strftime('%s','now'), NULL
FROM projects
WHERE allow_raw_ports = 1;
