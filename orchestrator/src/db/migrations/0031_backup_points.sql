-- User-named restore points: bookmarks on the continuous Litestream restore
-- window. Restore still goes through point-in-time recovery (`litestream
-- restore -timestamp`); the name is only a memorable label for that moment.
CREATE TABLE IF NOT EXISTS backup_points (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
