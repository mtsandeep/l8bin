-- Migration: Add nodes table and node_id to projects

CREATE TABLE IF NOT EXISTS nodes (
  id          TEXT    PRIMARY KEY,
  name        TEXT    NOT NULL,
  host        TEXT    NOT NULL,
  agent_port  INTEGER NOT NULL DEFAULT 8443,
  region      TEXT,
  status      TEXT    NOT NULL DEFAULT 'offline',
  total_memory INTEGER,
  total_cpu   REAL,
  last_seen_at INTEGER,
  fail_count  INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

-- SQLite does not support IF NOT EXISTS on ALTER TABLE.
-- This will be ignored if the column already exists (error swallowed by migration runner).
ALTER TABLE projects ADD COLUMN node_id TEXT REFERENCES nodes(id);
