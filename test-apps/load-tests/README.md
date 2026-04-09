# LiteBin Load Tests

k6 scripts for load testing the LiteBin orchestrator.

## Run Tests

`k6.exe` is included in this folder. Run from this directory:

```bash
cd test-apps/load-tests

# Realistic user flow (10 VUs, 40s)
./k6.exe run login-and-browse.js

# API stress test (50 VUs, 50s)
./k6.exe run api-stress.js

# Deploy 20 nginx apps
./k6.exe run deploy-nginx.js

# Deploy 20 SSR apps (requires pre-built image — see below)
./k6.exe run -e L8B_SSR_IMAGE=sha256:abcdef... deploy-ssr.js

# SSR load test — 200 users across 20 sites (10 per site)
./k6.exe run ssr-browse.js
```

### Custom VU count and duration

```bash
./k6.exe run --vus 100 --duration 60s login-and-browse.js
```

### With custom server and credentials

```bash
./k6.exe run -e L8B_BASE_URL=https://your-litebin.example.com -e L8B_PASSWORD=secret api-stress.js
```

### SSR load test options

```bash
# 20 sites, 5 users each = 100 VUs
./k6.exe run -e L8B_DOMAIN=localhost -e L8B_SITE_COUNT=20 -e L8B_USERS_PER_SITE=5 ssr-browse.js
```

## Config

Environment variables (all prefixed `L8B_` to avoid Windows collisions):

| Env Var | Default | Description |
|---------|---------|-------------|
| `L8B_BASE_URL` | `https://l8bin.localhost` | Dashboard/API URL |
| `L8B_USERNAME` | `admin` | Login username |
| `L8B_PASSWORD` | `passcode` | Login password |
| `L8B_DEPLOY_COUNT` | `20` | Number of apps to deploy |
| `L8B_SSR_IMAGE` | *(required)* | Pre-uploaded image ID (`sha256:...`) for SSR deploy |
| `L8B_SSR_PORT` | `3000` | Internal port for SSR app |
| `L8B_DOMAIN` | `localhost` | Domain for SSR site URLs |
| `L8B_SITE_COUNT` | `20` | Number of SSR sites for load test |
| `L8B_USERS_PER_SITE` | `10` | Concurrent users per site |

## What to Look For

- **p95 latency** — should be under 500ms for normal browse, under 1s under stress
- **Error rate** — should stay below 5%
- **`ssr_duration`** — custom metric for SSR product grid page
- **`detail_duration`** — custom metric for SSR product detail page
- **`stats_duration`** — `/projects/stats` metric; degrades with more running containers
- **Dashboard stack bar** — watch orchestrator RAM during tests

## SSR Load Test App

The Next.js SSR app is at `test-apps/nextjs-ssr-load/`. It generates heavy server-side rendered pages:

- `/` — 500 product cards with computed prices, ratings, stock (SSR)
- `/products/[id]` — Product detail with 20 reviews + 8 related items (SSR)
- `/api/health` — Lightweight health check

### Pre-build the SSR image

The SSR deploy test uses a **pre-built image** so the test focuses on load, not image creation. Build and upload once:

```bash
cd test-apps/nextjs-ssr-load

# Build + upload + deploy a single "template" instance
l8b deploy --project ssr-template --port 3000

# Note the image_id (sha256:...) from the output
```

Then run the deploy test with that image ID:

```bash
cd test-apps/load-tests

# Deploy 20 SSR instances using the pre-uploaded image
./k6.exe run -e L8B_SSR_IMAGE=sha256:<id-from-above> deploy-ssr.js

# Then hit them with load
./k6.exe run ssr-browse.js
```

The server skips registry pulls for `sha256:` images — it reuses the already-loaded image for every deployment.
