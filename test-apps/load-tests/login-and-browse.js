import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// --- Config (override via env vars) ---
const BASE_URL = __ENV.L8B_BASE_URL || 'https://l8bin.localhost';
const USERNAME = __ENV.L8B_USERNAME || 'admin';
const PASSWORD = __ENV.L8B_PASSWORD || 'passcode';

// --- Custom metrics ---
const errorRate = new Rate('errors');
const loginDuration = new Trend('login_duration');
const statsDuration = new Trend('stats_duration');

// --- Test options ---
export const options = {
  stages: [
    { duration: '10s', target: 10 },   // ramp up to 10 users
    { duration: '20s', target: 10 },   // hold 10 users
    { duration: '10s', target: 0 },    // ramp down
  ],
  thresholds: {
    http_req_duration: ['p(95)<500'],
    errors: ['rate<0.05'],
  },
};

export function setup() {
  // Login once to verify credentials work
  const res = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), {
    headers: { 'Content-Type': 'application/json' },
  });
  check(res, { 'setup login ok': (r) => r.status === 200 });
}

export default function () {
  // 1. Login
  const jar = http.cookieJar();
  jar.clear(BASE_URL);

  const loginRes = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), {
    headers: { 'Content-Type': 'application/json' },
  });
  loginDuration.add(loginRes.timings.duration);
  check(loginRes, { 'login 200': (r) => r.status === 200 }) || errorRate.add(1);

  // 2. Browse projects
  const projectsRes = http.get(`${BASE_URL}/projects`);
  check(projectsRes, { 'projects 200': (r) => r.status === 200 }) || errorRate.add(1);

  // 3. Fetch project stats (heaviest endpoint — hits Docker API per container)
  const statsRes = http.get(`${BASE_URL}/projects/stats`);
  statsDuration.add(statsRes.timings.duration);
  check(statsRes, { 'stats 200': (r) => r.status === 200 }) || errorRate.add(1);

  // 4. Fetch system stats
  const sysRes = http.get(`${BASE_URL}/system/stats`);
  check(sysRes, { 'system stats 200': (r) => r.status === 200 }) || errorRate.add(1);

  // 5. View a specific project (if any exist)
  if (projectsRes.status === 200) {
    const projects = projectsRes.json();
    if (projects.length > 0) {
      const id = projects[0].id;
      const detailRes = http.get(`${BASE_URL}/projects/${id}`);
      check(detailRes, { 'project detail 200': (r) => r.status === 200 }) || errorRate.add(1);
    }
  }

  // 6. Logout
  http.post(`${BASE_URL}/auth/logout`);

  // Simulate user think time
  sleep(Math.random() * 2 + 1);
}
