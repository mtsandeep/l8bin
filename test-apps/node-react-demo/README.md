# LiteBin Test App

A simple Express + React (Vite) demo app for testing the LiteBin PaaS platform.

## What This App Does

- **Express backend** serving a REST API on a configurable `PORT`
- **React frontend** (Vite) built into `dist/` and served as static files by Express
- **Health endpoint** at `GET /api/health` — returns uptime, status, timestamp
- **Info endpoint** at `GET /api/info` — returns app version, Node version, env, port
- **Items endpoint** at `GET /api/items` — returns a sample checklist

## Why This Exists

This project is purpose-built to test LiteBin's deployment pipeline:

1. **Railpack compatibility** — Standard Node.js project structure that Railpack can auto-detect
2. **Configurable PORT** — Reads `PORT` from environment (default: 3000)
3. **Health check** — `/api/health` can be used for container readiness probes
4. **Fast startup** — Boots in <1 second, ideal for testing cold starts
5. **Single process** — Production build is just `node server/index.js`

## Local Development

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

## Testing Scenarios

| Scenario | How to Test |
|:---|:---|
| **Deploy** | POST to `/deploy` with this app's image |
| **Health check** | `curl http://localhost:3000/api/health` |
| **Cold start** | Stop container, visit URL, measure wake time |
| **Idle sleep** | Deploy, wait 15+ min, verify container stops |
| **Port config** | Set `PORT=8080` env var, verify app listens there |

## GitHub Actions

The `.github/workflows/deploy.yml` file builds with Railpack and notifies LiteBin.

Required secrets:
- `LiteBin_URL` — Your LiteBin orchestrator URL (e.g. `https://api.yourdomain.com`)
- `DEPLOY_TOKEN` — Auth token for the `/deploy` endpoint
- `PROJECT_ID` — The subdomain/project ID (e.g. `demo-app`)
