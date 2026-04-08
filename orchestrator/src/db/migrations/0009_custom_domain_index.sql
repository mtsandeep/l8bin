CREATE INDEX IF NOT EXISTS idx_projects_custom_domain
    ON projects(custom_domain)
    WHERE custom_domain IS NOT NULL;
