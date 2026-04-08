import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// --- Config ---
const BASE_URL = __ENV.L8B_BASE_URL || 'http://localhost:5080';
const USERNAME = __ENV.L8B_USERNAME || 'admin';
const PASSWORD = __ENV.L8B_PASSWORD || 'passcode';

// --- Custom metrics ---
const errorRate = new Rate('errors');
const statsDuration = new Trend('stats_duration');

// --- Test options ---
// Higher load: ramp to 50 concurrent users hammering stats endpoints
export const options = {
  stages: [
    { duration: '10s', target: 50 },
    { duration: '30s', target: 50 },
    { duration: '10s', target: 0 },
  ],
  thresholds: {
    http_req_duration: ['p(95)<1000'],
    errors: ['rate<0.1'],
  },
};

export function setup() {
  const res = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), {
    headers: { 'Content-Type': 'application/json' },
  });
  check(res, { 'setup login ok': (r) => r.status === 200 });
}

export default function () {
  // Login
  const jar = http.cookieJar();
  jar.clear(BASE_URL);

  const loginRes = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), {
    headers: { 'Content-Type': 'application/json' },
  });
  if (!check(loginRes, { 'login 200': (r) => r.status === 200 })) {
    errorRate.add(1);
    sleep(1);
    return;
  }

  // Hammer the heaviest endpoints concurrently
  const responses = http.batch([
    ['GET', `${BASE_URL}/projects/stats`, null, { tags: { name: 'projects-stats' } }],
    ['GET', `${BASE_URL}/system/stats`, null, { tags: { name: 'system-stats' } }],
    ['GET', `${BASE_URL}/projects`, null, { tags: { name: 'projects-list' } }],
  ]);

  for (const res of responses) {
    if (res.url.includes('/projects/stats')) {
      statsDuration.add(res.timings.duration);
    }
    check(res, { 'response 200': (r) => r.status === 200 }) || errorRate.add(1);
  }

  // Minimal think time — this is a stress test
  sleep(0.5);
}
