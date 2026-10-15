#!/usr/bin/env python3
"""External observer for the resource-stress ramp — no SSH needed.

Writes to the output dir (default .):
  probe.log     dashboard latency + HTTP code, 1/s (code -1 = unreachable)
  api-probe.log /projects/stats API latency + code, every 5s
  stats.csv     per-container status + memory timeline via the API
  events.log    status changes (kill/deploy events) with timestamps

Usage: L8B_SERVER=https://... [L8B_TOKEN=...] python3 observe.py [outdir]
Stop with Ctrl-C. L8B_TOKEN only needs read scope.
"""
import csv
import json
import os
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request

SERVER = os.environ["L8B_SERVER"].rstrip("/")
TOKEN = os.environ.get("L8B_TOKEN", "")
OUT = sys.argv[1] if len(sys.argv) > 1 else "."
STATS_INTERVAL = 5
# curl -sk equivalent: skip TLS verification unless OBSERVE_VERIFY_SSL=1 (some
# test setups use self-signed certs; python also may lack the CA bundle on Windows).
CTX = ssl.create_default_context() if os.environ.get("OBSERVE_VERIFY_SSL") else ssl._create_unverified_context()


def fetch(path, auth=False, timeout=10):
    headers = {"Authorization": f"Bearer {TOKEN}"} if auth and TOKEN else {}
    req = urllib.request.Request(SERVER + path, headers=headers)
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=CTX) as r:
            return r.status, time.time() - t0, r.read(), ""
    except urllib.error.HTTPError as e:
        return e.code, time.time() - t0, None, ""
    except Exception as e:
        return -1, time.time() - t0, None, repr(e)[:140]


def probe_loop():
    with open(f"{OUT}/probe.log", "a", buffering=1) as f:
        while True:
            code, dt, _, err = fetch("/")
            line = f"{time.strftime('%H:%M:%S')} {code} {dt:.3f}"
            if code == -1 and err:
                line += f" {err}"
            f.write(line + "\n")
            time.sleep(1)


def iter_projects(data):
    items = data if isinstance(data, list) else data.get("projects", data.get("stats", []))
    for p in items or []:
        pid = str(p.get("project_id", ""))
        for s in p.get("services") or [p]:
            yield pid, s.get("service_name", "web"), s.get("status", "?"), int(s.get("memory_usage") or 0)


def stats_loop():
    prev = {}
    with open(f"{OUT}/stats.csv", "a", buffering=1, newline="") as csvf, open(f"{OUT}/events.log", "a", buffering=1) as ev, open(f"{OUT}/api-probe.log", "a", buffering=1) as ap:
        w = csv.writer(csvf)
        w.writerow(["time", "project", "service", "status", "mem_mb"])
        while True:
            ts = time.strftime("%H:%M:%S")
            code, dt, body, err = fetch("/projects/stats", auth=True)
            ap.write(f"{ts} {code} {dt:.3f}\n")
            if code != 200 or body is None:
                ev.write(f"{ts} API unreachable (HTTP {code} {err})\n")
            else:
                try:
                    rows = list(iter_projects(json.loads(body)))
                except Exception as e:
                    ev.write(f"{ts} stats parse error: {e}\n")
                    rows = []
                for pid, svc, status, mem in rows:
                    w.writerow([ts, pid, svc, status, mem // (1024 * 1024)])
                    key = f"{pid}/{svc}"
                    if key in prev and prev[key] != status:
                        ev.write(f"{ts} {key}: {prev[key]} -> {status} (at {mem // (1024 * 1024)}MB)\n")
                    prev[key] = status
            time.sleep(STATS_INTERVAL)


def main():
    os.makedirs(OUT, exist_ok=True)
    t1 = threading.Thread(target=probe_loop, daemon=True)
    t2 = threading.Thread(target=stats_loop, daemon=True)
    t1.start()
    t2.start()
    print(f"observing {SERVER} -> {OUT}/probe.log, stats.csv, events.log (Ctrl-C to stop)", flush=True)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
