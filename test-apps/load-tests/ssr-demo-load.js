import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// --- Config ---
const BASE_URL = __ENV.L8B_SSR_URL || 'https://nextjs-ssr-demo.l8b.in';

// --- Custom metrics ---
const errorRate = new Rate('errors');
const homeDuration = new Trend('home_duration');
const detailDuration = new Trend('detail_duration');

// --- Test options ---
export const options = {
  stages: [
    { duration: '10s', target: 50 },   // ramp up
    { duration: '120s', target: 50 },  // sustained load
    { duration: '10s', target: 0 },    // ramp down
  ],
  thresholds: {
    errors: ['rate<0.05'],
    http_req_duration: ['p(95)<5000'],
  },
};

const PRODUCT_COUNT = 100;

export default function () {
  const params = { headers: { Accept: 'text/html' } };

  // 1. Home page — SSR product grid with Suspense streaming
  const homeRes = http.get(`${BASE_URL}/`, { tags: { page: 'home' }, ...params });
  homeDuration.add(homeRes.timings.duration);
  check(homeRes, {
    'home 200': (r) => r.status === 200,
    'home has content': (r) => r.body.includes('SSR Test'),
  }) || errorRate.add(1);

  // 2. Random product detail — SSR with reviews + related products
  const productId = (__ITER % PRODUCT_COUNT) + 1;
  const detailRes = http.get(`${BASE_URL}/products/${productId}`, { tags: { page: 'detail' }, ...params });
  detailDuration.add(detailRes.timings.duration);
  check(detailRes, {
    'detail 200': (r) => r.status === 200,
    'detail has product': (r) => r.body.includes('product') || r.body.includes('Product'),
  }) || errorRate.add(1);

  // Think time between iterations
  sleep(Math.random() * 0.5 + 0.5);
}
