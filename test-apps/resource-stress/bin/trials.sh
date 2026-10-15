#!/usr/bin/env bash
# Timed canary deploy+delete trials — the management-action metric.
# Deploys a tiny 10%-CPU app and deletes it, timing both. Under a healthy
# box both complete in ~1s; under CPU/memory storms this is the number that
# shows whether the panel can still act (the "kill button" test).
# Usage: L8B_SERVER=https://... L8B_TOKEN=... ./trials.sh [count] [prefix]
set -euo pipefail

SERVER="${L8B_SERVER:?set L8B_SERVER=https://your-litebin-host}"
TOKEN="${L8B_TOKEN:?set L8B_TOKEN=<dashboard Settings > Access Tokens>}"
N="${1:-3}"
PREFIX="${2:-canary}"

mk() {
  printf '{"project_id":"%s","image":"polinux/stress","cmd":"stress --cpu 1 --timeout 600s","memory_limit_mb":64,"cpu_limit":0.1,"is_background":true}' "$1"
}

for i in $(seq 1 "$N"); do
  dep=$(curl -s -o /dev/null -w '%{time_total}' -m 150 -X POST \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d "$(mk "$PREFIX-$i")" "$SERVER/deploy")
  del=$(curl -s -o /dev/null -w '%{time_total}' -m 150 -X DELETE \
    -H "Authorization: Bearer $TOKEN" "$SERVER/projects/$PREFIX-$i")
  echo "trial $i: deploy=${dep}s delete=${del}s"
  sleep 2
done
