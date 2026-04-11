# Game Demo — Implementation Plan

## Context

We're building a team-based interactive game (Red vs Blue) as a LiteBin demo app. It deploys as **two projects**: a Postgres container and a Fastify + React app container. The game demonstrates PostgreSQL usage, resource monitoring, and DB auto-reset — while being fun and alive even with 1-5 users.

Source docs: `idea.md` (game mechanics & concept) and `game-flow.md` (UX flow).

## Deployment: Two LiteBin Projects

**Project 1: `game-demo-db`**
- Image: `postgres:16-alpine` (no custom Dockerfile)
- Port: 5432
- Container: `litebin-game-demo-db`
- Env (manual `.env`): `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB`

**Project 2: `game-demo`**
- Custom multi-stage Dockerfile (`node:24-alpine`)
- Port: 3000
- Container: `litebin-game-demo`
- Env (manual `.env`): `DATABASE_URL=postgres://user:pass@litebin-game-demo-db:5432/gamedemo`, `DB_SIZE_LIMIT_MB=50`

## Project Structure

```
test-apps/game-demo/
├── server/
│   ├── index.js            # Fastify entry, plugins, startup
│   ├── db.js               # PG connection pool, schema init, seed
│   ├── game-engine.js      # In-memory game state, tick loop, bot clicks
│   └── routes/
│       ├── game.js         # /api/state, /api/click, /api/minigame, /api/powerup
│       └── system.js       # /api/health, /api/stats (DB size, query counts)
├── src/
│   ├── main.jsx
│   ├── App.jsx             # Router: Landing → Intro → Game
│   ├── index.css           # Tailwind v4 + custom theme + game animations
│   ├── components/
│   │   ├── SystemBar.jsx   # Top bar: DB size, uptime, active users, status
│   │   ├── TeamPanel.jsx   # Red/Blue side panel with score + click button
│   │   ├── Counter.jsx     # Large central counter with animation
│   │   ├── LuckyZone.jsx   # Highlighted range on counter
│   │   ├── MiniGame.jsx    # Balloon/Math overlay
│   │   ├── Powerup.jsx     # Active powerup indicator
│   │   └── EventLog.jsx    # Bottom ticker: recent events
│   └── pages/
│       ├── Landing.jsx     # "Pick Your Side" (Red vs Blue cards)
│       └── Game.jsx        # Main game screen with all components
├── package.json
├── vite.config.js
├── Dockerfile
└── .dockerignore
```

## Database Schema

The Fastify app creates tables on startup (`CREATE TABLE IF NOT EXISTS`).

```sql
-- Game state snapshot (updated every few seconds by tick loop)
CREATE TABLE game_state (
  id INT PRIMARY KEY DEFAULT 1,
  counter INT DEFAULT 0,
  red_score INT DEFAULT 0,
  blue_score INT DEFAULT 0,
  updated_at TIMESTAMP DEFAULT NOW()
);

-- Click log (grows fastest — every click logged here)
CREATE TABLE click_log (
  id SERIAL PRIMARY KEY,
  team VARCHAR(4) NOT NULL,
  amount INT DEFAULT 1,
  source VARCHAR(10) DEFAULT 'user',  -- 'user' | 'bot'
  created_at TIMESTAMP DEFAULT NOW()
);

-- Game events (mini-game completions, powerups, resets)
CREATE TABLE game_events (
  id SERIAL PRIMARY KEY,
  event_type VARCHAR(30) NOT NULL,    -- 'minigame' | 'powerup' | 'reset' | 'lucky_zone'
  team VARCHAR(4),
  data JSONB DEFAULT '{}',
  created_at TIMESTAMP DEFAULT NOW()
);

-- System metrics snapshots (DB size over time)
CREATE TABLE system_metrics (
  id SERIAL PRIMARY KEY,
  db_size_bytes BIGINT,
  total_reads BIGINT DEFAULT 0,
  total_writes BIGINT DEFAULT 0,
  active_users INT DEFAULT 0,
  recorded_at TIMESTAMP DEFAULT NOW()
);
```

### Write Strategy: Batched Buffers

Clicks update the in-memory counter instantly (instant UI feedback). But instead of writing 1 INSERT per click to PG, we buffer clicks in a memory array and flush them as a single multi-row INSERT:

- Flush when buffer reaches **50 clicks** OR every **2 seconds** (whichever first)
- Uses `INSERT INTO click_log ... VALUES ($1,$2,$3,$4), ($5,$6,$7,$8), ...`
- Same number of rows, same DB growth, but **1 query per 50 clicks instead of 50 queries**
- Configurable via `CLICK_BATCH_SIZE` env var (default 50)

Game events and system metrics use the same batch approach. Game state snapshots are written every 5s via the tick loop.

## DB Size Monitoring & Auto-Reset

### Monitor (runs every 30s)

```js
async function checkDbSize() {
  const result = await db.query(`SELECT pg_database_size(current_database()) as size`);
  const sizeMb = result.rows[0].size / (1024 * 1024);
  const limitMb = parseInt(process.env.DB_SIZE_LIMIT_MB || '50');

  if (sizeMb >= limitMb * 0.95) {
    await resetDatabase();
  }

  // Insert metric snapshot
  await db.query(
    `INSERT INTO system_metrics (db_size_bytes, total_reads, total_writes, active_users)
     VALUES ($1, $2, $3, $4)`,
    [sizeBytes, reads, writes, users]
  );
}
```

### States shown in SystemBar

| State | Condition | UI |
|---|---|---|
| Green | < 70% of limit | Progress bar green, "Stable" |
| Yellow | 70-90% | Progress bar yellow, "Growing" |
| Red | 90-95% | Progress bar red, "Reset imminent" |
| Resetting | >= 95% | Full-width red, "Resetting database..." |

### Reset procedure

1. TRUNCATE `click_log`, `game_events`, `system_metrics`
2. RESET `game_state` to defaults (counter=0, scores=0)
3. Insert a `game_events` row with `event_type='reset'`
4. Reset in-memory query counters
5. Log reset timestamp

## API Endpoints

### Game Routes

| Method | Path | Description |
|---|---|---|
| GET | `/api/state` | Full game state + poll interval + system metrics |
| POST | `/api/click` | `{ team }` → log click, increment counter |
| POST | `/api/minigame` | `{ type, team, answer }` → validate, apply reward |
| POST | `/api/powerup/claim` | `{ team, type }` → activate powerup |

### System Routes

| Method | Path | Description |
|---|---|---|
| GET | `/api/health` | Uptime, memory, timestamp |
| GET | `/api/stats` | DB size, query counts, table row counts, last reset |

### `/api/state` Response (polled every 1-3s)

```json
{
  "counter": 1247,
  "teams": { "red": { "score": 450 }, "blue": { "score": 380 } },
  "luckyZone": { "active": true, "range": [1200, 1220], "team": "red", "timer": 5 },
  "miniGame": { "active": true, "type": "math", "challenge": "7 x 8", "timer": 8 },
  "powerup": { "active": false, "type": null, "team": null, "timer": 0 },
  "system": {
    "dbSizeMb": 12.4,
    "dbLimitMb": 50,
    "dbStatus": "green",
    "uptime": 3600,
    "activeUsers": 3,
    "totalClicks": 8923,
    "pollInterval": 1000
  }
}
```

## Game Engine (server-side)

### In-Memory State

- `counter`, `redScore`, `blueScore`
- `luckyZone` (range, assigned team, timer)
- `miniGame` (type, challenge, timer, active)
- `powerup` (type, team, timer, active)
- Query counters (`totalReads`, `totalWrites`)
- Active users tracking (by IP, 30s expiry)

### Write Buffers (batched DB writes)

All DB writes go through in-memory buffers that flush periodically:

- **Click buffer**: array of `{team, amount, source}` → flush at 50 items or 2s interval
- **Event buffer**: array of `{event_type, team, data}` → flush at 10 items or 3s interval
- **Metrics**: single snapshot → flush every 30s (tied to DB size check)
- **Game state**: single row → flush every 5s via tick loop

Each flush is a single multi-row INSERT. This gives us real DB growth with controlled write pressure.

### Tick Loop (every 200ms)

1. Update timers (lucky zone, mini-game, powerup)
2. Process lucky zone (check if counter in range)
3. Spawn/expire mini-games (10s on / 10s off cycle)
4. Spawn/expire powerups (random intervals)
5. Bot clicks (1-3 per tick to keep game alive)
6. Snapshot game state to DB every 5s (not every tick)
7. Flush click/event buffers when thresholds reached

### Bot System

Even with 0 real users, bots generate clicks and events. This keeps the game alive AND grows the DB. Configurable rate via `BOT_CLICK_RATE` env var.

## Frontend Pages

### Landing (`/`)

- Two large cards: "Join Red" / "Join Blue"
- Team stored in localStorage
- Subtle animated background (gradient or particles)
- "Waking system..." loading state (1-2s fake delay)

### Game (`/game`)

Layout:
```
+------------------------------------------+
|  SystemBar: DB 12.4/50MB | 3 active | 2h |
+----------+---------------+---------------+
|          |               |               |
|  RED     |   COUNTER     |    BLUE       |
|  450pts  |   1,247       |    380pts     |
|  [CLICK] |  [===LZ===]   |    [CLICK]   |
|          |               |               |
+----------+---------------+---------------+
|  Event ticker: "Red +50 from mini-game"  |
+------------------------------------------+
```

Overlays:
- Mini-game popup (balloon tap zone or math input)
- Powerup notification (fullscreen flash)
- DB reset banner

### Styling

- Tailwind CSS v4 (matching react-portfolio pattern)
- Dark theme with team accent colors (red-500, blue-500)
- `@theme` block: `--color-red-team`, `--color-blue-team`, `--font-mono`
- Animations: counter pulse on change, score float-up on gain, powerup flash

## Phased Implementation

### Phase 1: Core (working app)
- [ ] Fastify server with DB connection + schema init
- [ ] Click endpoint + click_log table
- [ ] In-memory game state + tick loop
- [ ] `/api/state` endpoint
- [ ] DB size monitor + auto-reset
- [ ] Landing page (pick team)
- [ ] Game page (counter + team panels + click)
- [ ] SystemBar (DB size, uptime)
- [ ] Dockerfile + .dockerignore
- [ ] Bot clicks for background activity

### Phase 2: Engagement (mini-games + lucky zone)
- [ ] Lucky zone mechanic
- [ ] Mini-games (balloon + math)
- [ ] Event ticker
- [ ] Better animations

### Phase 3: Disruption (powerups)
- [ ] Powerup system (freeze, fire, boost, reverse)
- [ ] Visual effects
- [ ] Throttling + adaptive polling

## Key Files to Reuse Patterns From

- `test-apps/node-react-demo/Dockerfile` — multi-stage build pattern
- `test-apps/node-react-demo/server/index.js` — ESM, static serving, SPA fallback
- `test-apps/react-portfolio/src/index.css` — Tailwind v4 @theme pattern
- `test-apps/react-portfolio/src/App.jsx` — React Router setup

## Verification

1. Run `pnpm dev:server` + `pnpm dev:client` locally (need local PG or Docker PG)
2. Verify click logging → check `click_log` table
3. Verify DB size tracking → `/api/stats`
4. Verify auto-reset → set `DB_SIZE_LIMIT_MB=1` for testing
5. Build Docker image: `docker build -t game-demo .`
6. Deploy both projects to LiteBin
7. Verify internal connectivity: app reaches `litebin-game-demo-db:5432`
