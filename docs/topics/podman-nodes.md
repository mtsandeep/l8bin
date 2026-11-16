# Podman Nodes (CRIU Checkpoint Sleep)

LiteBin talks to the Docker-compatible API, which [podman](https://podman.io) also
implements. Running a node on podman unlocks **checkpoint sleep**: idle apps are
frozen with [CRIU](https://criu.org) instead of stopped, and the next request
**resumes them in ~1 second with their memory intact** — no framework boot, no
lost in-process state.

| | Stop sleep (default) | Checkpoint sleep (podman + CRIU) |
|---|---|---|
| Idle resource use | 0 CPU, 0 RAM | 0 CPU, 0 RAM (state on disk ≈ app memory) |
| Wake time | 1–3 s + app boot (frameworks: 10–30 s) | ~1 s, boot skipped |
| App state on wake | Fresh process | Exact frozen state (counters, caches, warm pools) |
| Runtime | Any | podman + CRIU on the node |

## Who installs podman?

The **node admin, at node setup time** — this is a machine-level property, not a
dashboard toggle. LiteBin itself runs as containers on the node's runtime, so a
dashboard toggle cannot install or replace the host's container engine. The
toggle described below only *opts projects in*; it is a no-op (falls back to
plain stop) on nodes without podman.

## Setting up a podman node

On the node (Debian/Ubuntu; rootful podman is required — rootless cannot
publish ports the way litebin's proxy expects):

```bash
# 1. Install podman + CRIU
sudo apt update && sudo apt install -y podman criu

# 2. Enable the podman socket (serves the Docker-compatible API)
sudo systemctl enable --now podman.socket

# 3. Verify
sudo curl -s --unix-socket /run/podman/podman.sock http://d/v1.41/version | head
```

Then run the litebin agent with two changes compared to a Docker node:

1. Mount the podman socket instead of the Docker socket:
   `-v /run/podman/podman.sock:/run/podman/podman.sock`
2. Set the runtime env on the agent container: `CONTAINER_RUNTIME=podman`
   (`PODMAN_SOCKET` overrides the default `/run/podman/podman.sock` if you
   mount it elsewhere; `DOCKER_HOST` is honored if you prefer to point the
   compat client directly).

The agent reports the capability in its health heartbeat; the dashboard reads
it from the node list to enable the checkpoint option per project. The
orchestrator's own ("local") node can run on podman the same way — set
`CONTAINER_RUNTIME=podman` on the orchestrator container and mount the socket.

Installs and updates pull prebuilt images from GHCR (`ghcr.io/mtsandeep/l8bin/…`),
so no image builds happen on the server on either runtime. Set `L8B_NO_PULL=1`
to force the local-assembly path (offline installs, custom builds).

Building from a source checkout on podman: use `scripts/dev-up-podman.sh` —
it builds with podman's native builder (buildah) and starts the stack without
compose-side builds. Avoid `docker compose up --build` on podman: compose would
either try to boot a buildkitd container (unsupported) or fall back to the
deprecated classic build protocol (`DOCKER_BUILDKIT=0`).

## Enabling checkpoint sleep per project

Dashboard → project → **Sleep** popover → **Sleep mode: Checkpoint** (only
selectable when the project's node supports it). Via API:

```bash
curl -X PATCH https://your-litebin/api/projects/{id}/settings \
  -H 'Content-Type: application/json' \
  -d '{"sleep_mode": "checkpoint"}'
```

The setting is pushed to the agent over mTLS and persisted on the node, so
Mode B (agent-autonomous) wake restores checkpoints even when the master is
down.

## Behavior and guarantees

- **Sleep never fails** — if checkpointing fails (CRIU missing, exotic IPC,
  app state CRIU can't handle), the janitor falls back to a plain stop and
  logs a warning. The project still sleeps.
- **Wake falls back too** — restore is attempted first; if it doesn't apply
  (container was plain-stopped, checkpoint pruned), a normal start runs, and
  the existing recreate path remains the last resort.
- **Deploys/recreates reset the container** — a redeploy creates a fresh
  container; checkpoint state does not survive redeploys (same as stop mode).
- **Disk cost** — a sleeping project occupies disk roughly equal to its memory
  limit. Plan node storage accordingly for many checkpointed sleepers.
- **Env changes recreate** — like stop mode, a changed `.env` triggers a
  recreate on wake (the new environment can't be injected into a frozen
  process).
- **Stop semantics are explicit** — the dashboard's **Stop** button always
  plain-stops (predictable fresh boot on next start). Projects in checkpoint
  mode get a second button, **Freeze** (snowflake), which stops via CRIU
  checkpoint with state preserved. The idle janitor follows the project's
  sleep mode. Every variant wakes through the same restore-or-start path, so
  any stopped/checkpointed container comes back correctly.

## Verifying on a node

```bash
# Any running litebin container, checkpoint + resume by hand:
sudo podman container checkpoint litebin-myapp
sudo podman ps -a | grep myapp          # Exited (checkpointed)
sudo time podman container restore litebin-myapp
```

Restoring an app with a visible in-memory counter (a request counter, cache
hit meter) is the quickest end-to-end sanity check: the counter continues
from its frozen value instead of resetting.

## Limitations

- Linux nodes only (CRIU is a Linux feature; WSL2 kernels work for
  experimentation but some nftables-based podman networking features may not).
- Established TCP connections are checkpointed with `--tcp-established`
  (required — real servers hold keep-alive connections, which CRIU otherwise
  refuses to dump). Connections to peers outside the checkpoint (e.g. Caddy's
  keep-alive) die across the freeze; the app sees a reset on first use and
  reconnects, and the wake path re-proxies the held request, so visitors are
  unaffected.
- Checkpoint/restore has no Docker API equivalent — it is driven through
  podman's native libpod API on the same socket.
