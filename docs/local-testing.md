# Local Testing

## Prepare Local Release

Builds the orchestrator and dashboard, then creates a `local-release/` directory with correctly named files that `install.sh` expects.

```bash
bash setup/prepare-local-release.sh
```

This creates:
```
local-release/
  litebin-orchestrator-x86_64-linux
  l8b-dashboard-dist/
  checksums.txt
```

## Test Update Flow

```bash
L8B_RELEASE_DIR=./local-release bash install.sh update
```

Walk through the interactive prompts. You can say no to the restart at the end to verify the flow without actually restarting anything.

## Test Fresh Install

```bash
L8B_RELEASE_DIR=./local-release bash install.sh master
```

## Clean Up

```bash
rm -rf local-release/
```
