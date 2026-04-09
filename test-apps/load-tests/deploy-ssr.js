import http from 'k6/http';
import { check } from 'k6';

// --- Config ---
const BASE_URL = __ENV.L8B_BASE_URL || 'https://l8bin.localhost';
const USERNAME = __ENV.L8B_USERNAME || 'admin';
const PASSWORD = __ENV.L8B_PASSWORD || 'passcode';
const COUNT = parseInt(__ENV.L8B_DEPLOY_COUNT || '20');
const VU_COUNT = Math.min(COUNT, 5);
const PER_VU = Math.ceil(COUNT / VU_COUNT);

// Pre-uploaded image ID (sha256:...) from a prior `l8b deploy` run.
// Set L8B_SSR_IMAGE to the image_id returned by the upload step.
const IMAGE = __ENV.L8B_SSR_IMAGE;
const PORT = parseInt(__ENV.L8B_SSR_PORT || '3000');

if (!IMAGE) {
  console.error(
    'L8B_SSR_IMAGE is required. Pre-build and upload the image first:\n' +
    '  cd test-apps/nextjs-ssr-load\n' +
    '  l8b deploy --project ssr-template --port 3000\n' +
    'Then set L8B_SSR_IMAGE to the returned image ID (sha256:...).\n' +
    '  -e L8B_SSR_IMAGE=sha256:abcdef...'
  );
}

function login() {
  const res = http.post(`${BASE_URL}/auth/login`, JSON.stringify({
    username: USERNAME,
    password: PASSWORD,
  }), { headers: { 'Content-Type': 'application/json' } });
  check(res, { 'login ok': (r) => r.status === 200 });
  return res;
}

export function setup() {
  if (!IMAGE) {
    return;
  }
  login();
}

export default function (data) {
  if (!IMAGE) {
    return;
  }

  // Each VU handles a slice: VU1 → 1..4, VU2 → 5..8, etc.
  const index = (__VU - 1) * PER_VU + __ITER + 1;
  if (index > COUNT) return; // skip if COUNT isn't evenly divisible by VU_COUNT
  const id = `ssr-${String(index).padStart(3, '0')}`;

  login();

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

  // 2. Deploy using pre-uploaded image (sha256:... skips registry pull)
  const deployRes = http.post(`${BASE_URL}/deploy`, JSON.stringify({
    project_id: id,
    image: IMAGE,
    port: PORT,
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
      executor: 'per-vu-iterations',
      iterations: PER_VU,
      vus: VU_COUNT,
    },
  },
};
