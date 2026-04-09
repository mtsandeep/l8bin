import http from 'k6/http';
import { check } from 'k6';

// --- Config ---
const BASE_URL = __ENV.L8B_BASE_URL || 'https://l8bin.localhost';
const USERNAME = __ENV.L8B_USERNAME || 'admin';
const PASSWORD = __ENV.L8B_PASSWORD || 'passcode';
const COUNT = parseInt(__ENV.L8B_DEPLOY_COUNT || '20');
const VU_COUNT = Math.min(COUNT, 5);
const PER_VU = Math.ceil(COUNT / VU_COUNT);

function login() {
  const res = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), { headers: { 'Content-Type': 'application/json' } });
  check(res, { 'login ok': (r) => r.status === 200 });
  return res;
}

export function setup() {
  login();
}

export default function (data) {
  const index = (__VU - 1) * PER_VU + __ITER + 1;
  if (index > COUNT) return;
  const id = `ssr-${String(index).padStart(3, '0')}`;

  login();

  const res = http.del(`${BASE_URL}/projects/${id}`, null, {
    headers: { 'Content-Type': 'application/json' },
  });
  const ok = check(res, {
    [`delete ${id} ok`]: (r) => r.status === 200 || r.status === 204 || r.status === 404,
  });
  if (!ok) {
    console.log(`delete ${id}: ${res.status} ${res.body}`);
  } else {
    console.log(`deleted ${id}`);
  }
}

export const options = {
  scenarios: {
    cleanup: {
      executor: 'per-vu-iterations',
      iterations: PER_VU,
      vus: VU_COUNT,
    },
  },
};
