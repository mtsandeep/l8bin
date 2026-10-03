import express from "express";
import compression from "compression";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const app = express();
const PORT = process.env.PORT || 3000;
const startTime = Date.now();
let visitCount = 0;

app.use(compression());
app.use(express.json());

// Track visitors — one count per page load (extensionless, non-API paths).
// Keep-awake pings go to /api and are never counted.
app.use((req, _res, next) => {
  if (!req.path.startsWith("/api") && !path.extname(req.path)) {
    visitCount++;
  }
  next();
});

// --- API Routes ---

app.get("/api/health", (_req, res) => {
  res.json({
    status: "ok",
    uptime: Math.floor((Date.now() - startTime) / 1000),
    visitCount,
  });
});

app.get("/api/info", (_req, res) => {
  const mem = process.memoryUsage();
  res.json({
    app: "litebin-test-app",
    version: "1.0.0",
    node: process.version,
    env: process.env.NODE_ENV || "development",
    port: PORT,
    memory: {
      heapUsed: Math.round(mem.heapUsed / 1024 / 1024),
    },
  });
});

// --- Static Files (React build) ---

const clientDist = path.join(__dirname, "..", "dist");
app.use(express.static(clientDist));

// SPA fallback — serve index.html for any non-API route
app.get("*", (_req, res) => {
  res.sendFile(path.join(clientDist, "index.html"));
});

app.listen(PORT, () => {
  console.log(`litebin-test-app listening on port ${PORT}`);
});
