-- Multi-service support: project_services + project_volumes tables
-- Existing single-service projects are migrated to a single "web" service row.

CREATE TABLE IF NOT EXISTS project_services (
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_name    TEXT NOT NULL,
    image           TEXT NOT NULL,
    port            INTEGER,
    cmd             TEXT,
    is_public       INTEGER NOT NULL DEFAULT 0,
    depends_on      TEXT,
    container_id    TEXT,
    mapped_port     INTEGER,
    memory_limit_mb INTEGER,
    cpu_limit       REAL,
    status          TEXT NOT NULL DEFAULT 'stopped',
    instance_id     TEXT DEFAULT NULL,
    PRIMARY KEY (project_id, service_name)
);

CREATE TABLE IF NOT EXISTS project_volumes (
    project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_name   TEXT NOT NULL,
    volume_name    TEXT,
    container_path TEXT NOT NULL,
    PRIMARY KEY (project_id, service_name, container_path)
);

-- Denormalized fields on projects for the 5s poll (no JOINs)
ALTER TABLE projects ADD COLUMN service_count INTEGER DEFAULT 1;
ALTER TABLE projects ADD COLUMN service_summary TEXT;

-- Migrate existing single-service projects into project_services
INSERT INTO project_services (project_id, service_name, image, port, is_public,
                              container_id, mapped_port, status, cmd, memory_limit_mb, cpu_limit)
SELECT id, 'web', image, internal_port, 1, container_id, mapped_port, status, cmd, memory_limit_mb, cpu_limit
FROM projects WHERE image IS NOT NULL;

-- Update denormalized summary fields
UPDATE projects SET service_count = 1, service_summary = 'web' || COALESCE(':' || internal_port, '') WHERE image IS NOT NULL;

-- Indexes for common query patterns
CREATE INDEX IF NOT EXISTS idx_project_services_project ON project_services(project_id);
CREATE INDEX IF NOT EXISTS idx_project_services_status ON project_services(status);
CREATE INDEX IF NOT EXISTS idx_project_volumes_project ON project_volumes(project_id);
