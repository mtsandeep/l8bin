-- Mark Compose one-shot jobs (service_completed_successfully deps).
-- Completed jobs stay exited and must not make the project "degraded".
ALTER TABLE project_services ADD COLUMN is_oneshot INTEGER NOT NULL DEFAULT 0;
