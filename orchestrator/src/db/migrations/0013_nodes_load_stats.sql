-- Store load stats from agent health reports for node selection.
ALTER TABLE nodes ADD COLUMN available_memory INTEGER;
ALTER TABLE nodes ADD COLUMN disk_free INTEGER;
ALTER TABLE nodes ADD COLUMN container_count INTEGER NOT NULL DEFAULT 0;
