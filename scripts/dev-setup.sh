#!/usr/bin/env bash
# One-time per-clone dev setup: point git at the shared hooks in .githooks/
# so every commit runs rustfmt + clippy + biome on the staged files.
set -e
cd "$(git rev-parse --show-toplevel)"

git config core.hooksPath .githooks
echo "OK: core.hooksPath=.githooks — pre-commit now runs rustfmt, clippy, and biome on staged files"
