-- Node capability: whether the node's container runtime supports CRIU
-- checkpoint/restore (podman). Reported via agent health / local runtime
-- detection; drives the dashboard's per-project checkpoint sleep option.
ALTER TABLE nodes ADD COLUMN supports_checkpoint INTEGER NOT NULL DEFAULT 0;
