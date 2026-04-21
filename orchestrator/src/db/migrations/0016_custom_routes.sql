CREATE TABLE IF NOT EXISTS project_routes (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    route_type TEXT NOT NULL CHECK(route_type IN ('path', 'alias')),
    path TEXT,
    subdomain TEXT,
    upstream TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 100,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_project_routes_project ON project_routes(project_id);
CREATE INDEX IF NOT EXISTS idx_project_routes_alias ON project_routes(subdomain);
