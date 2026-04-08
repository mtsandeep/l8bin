-- Global key/value settings table
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Per-project resource overrides (NULL = use global default)
ALTER TABLE projects ADD COLUMN memory_limit_mb INTEGER;
ALTER TABLE projects ADD COLUMN cpu_limit REAL;
