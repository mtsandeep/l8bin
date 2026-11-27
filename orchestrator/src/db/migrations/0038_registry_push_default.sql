-- Track which registry (if any) is the default push target for `l8b build --push` / `l8b deploy`.
ALTER TABLE registries ADD COLUMN is_push_default INTEGER NOT NULL DEFAULT 0;
