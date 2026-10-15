# Resource-stress test suite

Load/stress tests for [resource protection](../../docs/architecture/resource-protection.md): drive a LiteBin node into CPU/memory exhaustion with disposable stress apps and measure what happens to the platform. The full campaign narrative and findings live in [RESULTS.md](RESULTS.md); `bin/report.py` regenerates the summary tables from the raw data.

## Layout

| file | role |
|---|---|
| `bin/ramp.sh` | deploy/cleanup fleets of stress apps via the API (`cleanup` mode deletes everything) |
| `bin/trials.sh` | timed canary deploy+delete — the "does the kill button work" metric |
| `bin/observe.py` | external observer: `probe.log` (dashboard latency, 1/s), `api-probe.log` (API latency, 5s), `stats.csv` (per-container state), `events.log` (stop/deploy events) — no SSH needed |
| `bin/measure.js` | k6 percentile measurement (1 VU, 2 min, keep-alive; per-route sub-metrics) |
| `bin/report.py` | regenerates `REPORT.md` + `report.html` from run dirs and k6 JSONs |
| `runs/` (observer dirs + k6 JSONs) | raw data from past runs |

All tools take `L8B_SERVER` + `L8B_TOKEN` (dashboard → Settings → Access Tokens; admin scope — deploy alone can't clean up).

## Test catalog

| test | measures | command |
|---|---|---|
| Memory ramp — hogs | OOM cliff, victim selection, dashboard degradation | `VM_MB=480 LIMIT_MB=512 COUNT=8 PACE=90 ./bin/ramp.sh` |
| Memory ramp — realistic fleet | same, at typical app sizes (~100MB each) | `VM_MB=100 LIMIT_MB=128 COUNT=28 SETTLE=10 PACE=15 FUSE_SECS=2400 ./bin/ramp.sh` |
| CPU storm | management-plane responsiveness under pure CPU oversubscription | `WORKLOAD=cpu LIMIT_MB=64 COUNT=20 SETTLE=10 PACE=15 FUSE_SECS=2400 ./bin/ramp.sh` |
| Action trials | canary deploy/delete latency under whatever load is running | `./bin/trials.sh 5` |
| Percentiles | k6 p50/p90/p95/max for `/` and `/projects/stats` | `L8B_SERVER=... L8B_TOKEN=... k6 run --summary-export runs/<name>-k6.json bin/measure.js` |
| Disaster recreation | the full failure: memory-churn fleet at severity + action trials through it | fleet ramp, then `./bin/trials.sh 3` at peak |

`COUNT`/`PACE` also work as positional args (`./bin/ramp.sh 8 90`). Every stress app carries a self-destruct fuse (`FUSE_SECS`, default 4h — use 2400 or less on small boxes) so a lost session self-heals.

## Running a session

```bash
L8B_SERVER=... L8B_TOKEN=... python bin/observe.py runs/<run-name>   # terminal 1, runs all session
L8B_SERVER=... L8B_TOKEN=... ./bin/ramp.sh ...                  # terminal 2 (or batches)
./bin/trials.sh 5                                               # at each interesting state
L8B_SERVER=... L8B_TOKEN=... k6 run --summary-export <run-name>-k6.json measure.js
L8B_SERVER=... L8B_TOKEN=... ./bin/ramp.sh cleanup              # teardown
python bin/report.py                                            # regenerate REPORT.md / report.html
```

One run directory per phase — stop the observer between phases (a directory that spans several phases produces meaningless aggregates; see the excluded `prot-2g3-small` in `bin/report.py`). Label new directories in `bin/report.py`'s `RUNS` map.

## Config toggles (the matrix axes)

Protection off/on (SSH on the node):

```bash
cd /opt/litebin
mv docker-compose.override.yml docker-compose.override.yml.bak && docker compose up -d   # OFF (adj=0)
mv docker-compose.override.yml.bak docker-compose.override.yml && docker compose up -d   # ON (adj=-300)
# daemon lane: systemctl set-property docker.service containerd.service CPUWeight=100    # OFF
#              systemctl set-property docker.service CPUWeight=2000 MemoryLow=150M ...   # ON
```

Swap resize: `swapoff /swapfile && rm /swapfile && fallocate -l <SIZE> /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile` (keep the same path for fstab).

## Safety and caveats

- Disposable VPS only, provider console accessible; storms are deliberately destructive.
- Shared-CPU VPS: sustained 100% CPU can trip provider fair-use throttling — watch `vmstat 1` steal (`st`), and treat absolute latencies as directional (mechanism conclusions are robust to it; benchmark-grade numbers want a dedicated-vCPU box).
- `events.log` stop events include OOM kills, cleanup deletes, and janitor idle-stops — the OOM kills are identified narratively in RESULTS.md.
- On the VPS, kernel OOM victim statements persist in dmesg: `ssh ... 'dmesg -T | grep -i "killed process"'` recovers them after a run.
