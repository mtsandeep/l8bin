import http from 'k6/http';
import { check } from 'k6';

// --- Config ---
const BASE_URL = __ENV.L8B_BASE_URL || 'http://localhost:5080';
const USERNAME = __ENV.L8B_USERNAME || 'admin';
const PASSWORD = __ENV.L8B_PASSWORD || 'passcode';
const COUNT = parseInt(__ENV.L8B_DEPLOY_COUNT || '20');

function login() {
  const res = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), { headers: { 'Content-Type': 'application/json' } });
  check(res, { 'login ok': (r) => r.status === 200 });
}

export default function () {
  // Login fresh for each VU
  login();

  // __ITER resets per VU, so combine VU+ITER for global uniqueness
  const index = (__VU - 1) * 100 + __ITER + 1;
  const id = `nginx-${String(index).padStart(3, '0')}`;

  // 1. Create project
  const createRes = http.post(`${BASE_URL}/projects`, JSON.stringify({ id }), {
    headers: { 'Content-Type': 'application/json' },
  });
  const created = check(createRes, {
    [`create ${id} ok`]: (r) => r.status === 200 || r.status === 201 || r.status === 409,
  });
  if (!created) {
    console.log(`create ${id}: ${createRes.status} ${createRes.body}`);
    return;
  }

  // 2. Deploy nginx
  const deployRes = http.post(`${BASE_URL}/deploy`, JSON.stringify({
    project_id: id,
    image: 'nginx:alpine',
    port: 80,
  }), {
    headers: { 'Content-Type': 'application/json' },
  });
  const deployed = check(deployRes, {
    [`deploy ${id} ok`]: (r) => r.status === 200,
  });
  if (!deployed) {
    console.log(`deploy ${id}: ${deployRes.status} ${deployRes.body}`);
  } else {
    console.log(`deployed ${id}`);
  }
}

export const options = {
  scenarios: {
    deploy: {
      executor: 'shared-iterations',
      iterations: COUNT,
      vus: Math.min(COUNT, 5),
    },
  },
};
