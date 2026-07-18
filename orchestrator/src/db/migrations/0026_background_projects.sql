-- Explicit project-level workload exposure. Existing projects remain web projects.
ALTER TABLE projects
ADD COLUMN is_background INTEGER NOT NULL DEFAULT 0 CHECK (is_background IN (0, 1));
