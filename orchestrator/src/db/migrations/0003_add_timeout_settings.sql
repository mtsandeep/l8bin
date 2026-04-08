-- Migration: Add per-project timeout settings to projects table

ALTER TABLE projects ADD COLUMN auto_stop_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE projects ADD COLUMN auto_stop_timeout_mins INTEGER NOT NULL DEFAULT 15;
ALTER TABLE projects ADD COLUMN auto_start_enabled INTEGER NOT NULL DEFAULT 1;
