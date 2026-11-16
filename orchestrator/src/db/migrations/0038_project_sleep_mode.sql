-- Per-project idle sleep mode: 'stop' (classic) or 'checkpoint' (CRIU
-- checkpoint/restore on podman nodes; falls back to stop elsewhere).
ALTER TABLE projects ADD COLUMN sleep_mode TEXT NOT NULL DEFAULT 'stop';
