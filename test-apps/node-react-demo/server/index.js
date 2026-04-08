import express from "express";
import compression from "compression";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const app = express();
const PORT = process.env.PORT || 3000;
const startTime = Date.now();

app.use(compression());
app.use(express.json());

// --- API Routes ---

app.get("/api/health", (_req, res) => {
  res.json({
    status: "ok",
    uptime: Math.floor((Date.now() - startTime) / 1000),
    timestamp: new Date().toISOString(),
  });
});

app.get("/api/info", (_req, res) => {
  res.json({
    app: "litebin-test-app",
    version: "1.0.0",
    node: process.version,
    env: process.env.NODE_ENV || "development",
    port: PORT,
  });
});

app.get("/api/items", (_req, res) => {
  res.json([
    { id: 1, name: "Deploy a Node app", done: true },
    { id: 2, name: "Auto-sleep idle containers", done: false },
    { id: 3, name: "Wake on first request", done: false },
    { id: 4, name: "Provision TLS automatically", done: false },
  ]);
});

// --- Static Files (React build) ---

const clientDist = path.join(__dirname, "..", "dist");
app.use(express.static(clientDist));

// SPA fallback — serve index.html for any non-API route
app.get("*", (_req, res) => {
  res.sendFile(path.join(clientDist, "index.html"));
});

app.listen(PORT, () => {
  console.log(`🚀 litebin-test-app listening on port ${PORT}`);
});
