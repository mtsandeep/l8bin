// Percentile measurement for the resource-stress comparison — p50/p95/p99
// for the cheap route (/) and the heavy API route (/projects/stats).
// Usage: L8B_SERVER=https://... L8B_TOKEN=... k6 run --summary-export out.json measure.js
import http from 'k6/http';
import { sleep } from 'k6';

const BASE = __ENV.L8B_SERVER;
const TOKEN = __ENV.L8B_TOKEN || '';

export const options = {
  vus: 1,
  duration: '120s',
  thresholds: {
    // empty thresholds force per-route sub-metrics into the summary/export
    'http_req_duration{route:root}': [],
    'http_req_duration{route:api}': [],
  },
};

export default function () {
  http.get(`${BASE}/`, { tags: { route: 'root' } });
  http.get(`${BASE}/projects/stats`, {
    headers: { Authorization: `Bearer ${TOKEN}` },
    tags: { route: 'api' },
  });
  sleep(1);
}
