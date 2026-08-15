# LiteBin Dashboard

Web UI for managing LiteBin: deploying single-image and compose apps, starting/stopping/sleeping projects, logs, per-project stats, multi-node management, scan-and-import, and global settings (domain/DNS, routing mode, deploy tokens).

Talks to the orchestrator's HTTP API (see `docs/api-reference.md`); all calls go through the central client in `src/api.ts`.

## Current state: deliberately simple

The dashboard is intentionally kept simple and raw for now. The priority is getting core functionality working and the UI visually usable — not frontend polish. Consequences you'll notice in the code:

- Feature screens are single-file components (some are large) rather than decomposed component trees.
- No shared UI primitive layer — modal/form markup is repeated inline.
- No test suite yet.
- State management is plain hooks + contexts with direct polling; no data-fetching/caching library.

Once the core feature set settles, the plan is to revisit and potentially move to a better dashboard implementation (or refactor this one properly) — including shared primitives, tests, and deeper routing.

## Stack

- React 19 + TypeScript (strict), Vite, Tailwind CSS 4
- Biome for lint/format, simple-git-hooks + nano-staged pre-commit
