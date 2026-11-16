-- Whether a stopped project's containers were CRIU-checkpointed (podman).
-- Presentational: "Frozen" badge instead of "Stopped" on the dashboard;
-- wake handles stopped and frozen containers identically (restore-or-start).
-- Cleared by any status transition back to Running/Deploying/Importing.
ALTER TABLE projects ADD COLUMN frozen INTEGER NOT NULL DEFAULT 0;
