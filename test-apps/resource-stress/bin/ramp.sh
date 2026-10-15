#!/usr/bin/env bash
# Memory-exhaustion ramp — deploys stress-N apps via the LiteBin API.
# Usage: L8B_SERVER=https://... L8B_TOKEN=... ./ramp.sh [count] [interval_secs]
#        L8B_SERVER=... L8B_TOKEN=... ./ramp.sh cleanup
set -euo pipefail

SERVER="${L8B_SERVER:?set L8B_SERVER=https://your-litebin-host}"
TOKEN="${L8B_TOKEN:?set L8B_TOKEN=<dashboard Settings > Access Tokens>}"
MODE="${1:-${COUNT:-6}}"
INTERVAL="${2:-${PACE:-90}}"
VM_MB="${VM_MB:-480}"
LIMIT_MB="${LIMIT_MB:-512}"
SETTLE="${SETTLE:-60}"
FUSE_SECS="${FUSE_SECS:-14400}"
WORKLOAD="${WORKLOAD:-vm}"   # vm = memory churn | cpu = pure CPU spinners
if [ "$WORKLOAD" = "cpu" ]; then
  CMD="stress --cpu 2 --timeout ${FUSE_SECS}s"
else
  CMD="stress --vm 1 --vm-bytes ${VM_MB}M --timeout ${FUSE_SECS}s"
fi

api() { curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" "$@"; }

stress_stats() {
  api "$SERVER/projects/stats" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for p in (d if isinstance(d, list) else d.get('projects', d.get('stats', []))):
    pid = str(p.get('project_id', ''))
    if pid.startswith('stress-'):
        svc = p.get('services') or [p]
        mem = sum(s.get('memory_usage', 0) or 0 for s in svc if isinstance(s, dict))
        state = svc[0].get('status', p.get('status', '?')) if svc else '?'
        print(f\"{pid}: {state} {mem//(1024*1024)}MB\")
" 2>/dev/null || true
}

if [ "$MODE" = "cleanup" ]; then
  for i in $(seq 1 128); do
    code=$(api -o /dev/null -w '%{http_code}' -X DELETE "$SERVER/projects/stress-$i")
    [ "$code" = "200" ] && echo "deleted stress-$i" || break
  done
  exit 0
fi

for i in $(seq 1 "$MODE"); do
  echo "[$(date +%T)] deploying stress-$i"
  api -X POST -d "{\"project_id\":\"stress-$i\",\"image\":\"polinux/stress\",\"cmd\":\"$CMD\",\"memory_limit_mb\":$LIMIT_MB,\"cpu_limit\":0.5,\"is_background\":true}" "$SERVER/deploy"
  echo
  sleep "$SETTLE"
  stress_stats
  [ "$i" -lt "$MODE" ] && sleep "$INTERVAL"
done

echo "[$(date +%T)] ramp complete — final state:"
stress_stats
