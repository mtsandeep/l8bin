#!/bin/bash
# Dev helper: run the litebin master stack on a local podman runtime.
#
# Builds images with podman's native builder (buildah) and then starts the
# stack via docker compose WITHOUT compose-side builds — avoiding the
# deprecated classic docker-build protocol entirely.
#
# Usage: scripts/dev-up-podman.sh [extra compose args, e.g. --force-recreate]
set -euo pipefail
cd "$(dirname "$0")/.."

export DOCKER_HOST="${DOCKER_HOST:-unix:///run/podman/podman.sock}"

if ! podman info >/dev/null 2>&1; then
  echo "podman unreachable ($DOCKER_HOST). Enable the socket:" >&2
  echo "  sudo systemctl enable --now podman.socket" >&2
  exit 1
fi
command -v docker >/dev/null || { echo "docker CLI (compose v2) required" >&2; exit 1; }

echo "==> Building litebin-orchestrator (podman build)..."
podman build -q -t litebin-orchestrator -f orchestrator/Dockerfile .

echo "==> Building litebin-dashboard (podman build)..."
podman build -q -t litebin-dashboard -f dashboard/Dockerfile .

echo "==> Starting stack (docker compose, no build)..."
exec docker compose \
  -f docker-compose.yml \
  -f docker-compose.override.yml \
  -f docker-compose.podman.yml \
  --profile master up -d "$@"
