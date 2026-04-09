#!/usr/bin/env bash
set -euo pipefail

# Prepare a local release directory for testing install.sh update flow.
# Usage: bash setup/prepare-local-release.sh
#
# Creates ./local-release/ with correctly named files that install.sh expects:
#   - litebin-orchestrator-x86_64-linux
#   - l8b-dashboard-dist/
#   - checksums.txt
#
# Then test with: L8B_RELEASE_DIR=./local-release bash install.sh update

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RELEASE_DIR="${PROJECT_DIR}/local-release"

echo "==> Building orchestrator..."
cargo build --release -p litebin-orchestrator

echo "==> Building dashboard..."
(cd "${PROJECT_DIR}/dashboard" && npm run build)

echo "==> Preparing release directory: ${RELEASE_DIR}"
rm -rf "$RELEASE_DIR"
mkdir -p "${RELEASE_DIR}/l8b-dashboard-dist"

# Copy orchestrator binary with release naming
cp "${PROJECT_DIR}/target/release/litebin-orchestrator" \
   "${RELEASE_DIR}/litebin-orchestrator-x86_64-linux" 2>/dev/null || \
cp "${PROJECT_DIR}/target/release/litebin-orchestrator.exe" \
   "${RELEASE_DIR}/litebin-orchestrator-x86_64-linux"

# Copy dashboard dist
cp -r "${PROJECT_DIR}/dashboard/dist/." "${RELEASE_DIR}/l8b-dashboard-dist/"

# Generate checksums
(cd "$RELEASE_DIR" && sha256sum litebin-orchestrator-x86_64-linux > checksums.txt)

echo ""
echo "==> Local release ready at ${RELEASE_DIR}"
echo ""
echo "  Files:"
ls -lh "$RELEASE_DIR"
echo ""
echo "  Test update flow:"
echo "    L8B_RELEASE_DIR=./local-release bash install.sh update"
