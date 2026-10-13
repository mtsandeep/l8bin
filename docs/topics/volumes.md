# Volumes & Bind Mounts

## Volume Types

LiteBin supports three volume types, determined by the source name:

| Type | Source format | Resolved to | Example | Created by |
|---|---|---|---|---|
| Docker named volume | `pgdata` | `litebin_{project_id}_pgdata` | `litebin_quiz_pgdata` | Docker |
| Relative bind mount | `./data` | `projects/{project_id}/data` | `projects/quiz/data` | LiteBin (directory) |
| Absolute bind mount | `/host/path` | `/host/path` (unchanged) | `/mnt/ssd/uploads` | User |

## Scoping Rules

- **Named volumes** are prefixed with `litebin_{project_id}_` to avoid cross-project conflicts. This prefix also makes all LiteBin volumes easily identifiable via `docker volume ls | grep litebin`.
- **Relative bind mounts** (`./`) are resolved relative to the `projects/{project_id}/` directory on the host. This keeps project data organized in one place.
- **Absolute bind mounts** (`/`) are passed through unchanged. The user is responsible for managing these paths.

## Naming Convention

Docker volume names follow the pattern: `litebin_{project_id}_{volume_name}`

Example: a project with ID `quiz` and a volume named `pgdata` becomes `litebin_quiz_pgdata`.

This is consistent with how Docker Compose scopes volumes (`{project}_{volume_name}`), with an additional `litebin_` platform prefix.

## How Each Path Handles Volumes

### Deploy

| Path | What happens |
|---|---|
| Single-service (web) | `from_project()` calls `scope_volume_source()` to resolve each volume name |
| Multi-service (compose) | `scope_volume_name()` in `compose_run.rs` resolves each compose volume spec |

### Delete

| Path | What happens |
|---|---|
| Local single-service | `cleanup_project_resources()` removes containers, volumes, network, project dir |
| Local multi-service | Same `cleanup_project_resources()` via `delete_all_services()` |
| Remote (agent) | Orchestrator calls agent `POST /containers/cleanup`, agent runs `cleanup_project_resources()` locally |

### Volume Cleanup Behavior on Delete

| Volume Type | On Delete |
|---|---|
| Docker named volume | `docker volume rm` |
| Relative bind mount | `rm -rf projects/{project_id}/data` |
| Absolute bind mount | Skipped (user-managed) |

## Compose Examples

```yaml
services:
  db:
    image: postgres:16
    volumes:
      # Docker named volume → litebin_myproject_pgdata
      - pgdata:/var/lib/postgresql/data

      # Relative bind mount → projects/myproject/backups
      - ./backups:/backups

      # Absolute bind mount → /mnt/ssd/uploads (unchanged)
      - /mnt/ssd/uploads:/uploads
```

## Relevant Code

| Component | File | Purpose |
|---|---|---|
| `scope_volume_source()` | `litebin-common/src/types.rs` | Resolves volume source name to final form |
| `classify_volume()` | `litebin-common/src/types.rs` | Classifies a scoped name for cleanup strategy |
| `scope_volume_name()` | `litebin-common/src/compose_run.rs` | Applies scoping to compose volume specs |
| `remove_volume_by_name()` | `litebin-common/src/docker.rs` | Removes a volume by classified type |
| `cleanup_project_resources()` | `litebin-common/src/docker.rs` | Full project resource teardown |
| `POST /containers/cleanup` | `agent/src/routes/containers.rs` | Agent endpoint for remote cleanup |
