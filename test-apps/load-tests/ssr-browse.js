import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// --- Config ---
const BASE_DOMAIN = __ENV.L8B_DOMAIN || 'localhost';
const SITE_COUNT = parseInt(__ENV.L8B_SITE_COUNT || '20');
const USERS_PER_SITE = parseInt(__ENV.L8B_USERS_PER_SITE || '10');

// --- Custom metrics ---
const errorRate = new Rate('errors');
const ssrDuration = new Trend('ssr_duration');
const detailDuration = new Trend('detail_duration');

// --- Test options ---
// Total VUs = users_per_site × site_count (e.g. 10 × 20 = 200)
export const options = {
  stages: [
    { duration: '15s', target: SITE_COUNT * USERS_PER_SITE },  // ramp up
    { duration: '60s', target: SITE_COUNT * USERS_PER_SITE },  // sustained load
    { duration: '15s', target: 0 },                              // ramp down
  ],
  thresholds: {
    http_req_duration: ['p(95)<10000'],
    errors: ['rate<0.1'],
  },
};

// Each VU is assigned to a specific site based on its VU ID
function getSiteId(vuId) {
  const siteIndex = ((vuId - 1) % SITE_COUNT) + 1;
  return `ssr-${String(siteIndex).padStart(3, '0')}`;
}

export default function () {
  const site = getSiteId(__VU);
  const hostHeader = `${site}.${BASE_DOMAIN}`;
  // k6 on Windows can't resolve *.localhost, so connect to 127.0.0.1 with Host header for Caddy routing
  const baseUrl = 'http://127.0.0.1';
  const params = { headers: { Host: hostHeader }, tags: { site } };

  // 1. Hit the heavy SSR product grid (500 products rendered server-side)
  const homeRes = http.get(`${baseUrl}/`, Object.assign({}, params, { tags: { page: 'home', site } }));
  ssrDuration.add(homeRes.timings.duration);
  check(homeRes, {
    'home 200': (r) => r.status === 200,
  }) || errorRate.add(1);

  // 2. Hit a random product detail page (SSR with reviews + related)
  const productId = (__ITER % 500) + 1;
  const detailRes = http.get(`${baseUrl}/products/${productId}`, Object.assign({}, params, { tags: { page: 'detail', site } }));
  detailDuration.add(detailRes.timings.duration);
  check(detailRes, {
    'detail 200': (r) => r.status === 200,
  }) || errorRate.add(1);

  // 3. Health check (lightweight)
  const healthRes = http.get(`${baseUrl}/api/health`, Object.assign({}, params, { tags: { page: 'health', site } }));
  check(healthRes, {
    'health 200': (r) => r.status === 200,
  }) || errorRate.add(1);

  // Think time
  sleep(Math.random() * 2 + 1);
}
