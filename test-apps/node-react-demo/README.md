# LiteBin Test App

A simple Express + React (Vite) demo app.

## Features

- **Express backend** serving a REST API on a configurable `PORT`
- **React frontend** (Vite) built into `dist/` and served as static files by Express
- **Live idle timer** — shows time until the container sleeps (60s of inactivity)
- **Health check** — `GET /api/health` — uptime, status, timestamp, lastVisitorAgo
- **Server info** — `GET /api/info` — app version, Node version, env, port
- **Checklist** — `GET /api/items` — sample checklist items

## Getting Started

```bash
# Install dependencies
pnpm install

# Run frontend dev server (with API proxy to backend)
pnpm dev:client

# Run backend (in another terminal)
pnpm dev:server

# Build for production
pnpm build

# Start production server (serves built frontend + API)
pnpm start
```

## Production

The Dockerfile uses a multi-stage build. The production image runs `NODE_ENV=production node server/index.js` and serves the built frontend from `dist/`.

The app reads `PORT` from the environment (default: 3000).
