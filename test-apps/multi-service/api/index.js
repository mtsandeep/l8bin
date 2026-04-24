import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import Fastify from "fastify";
import cors from "@fastify/cors";
import pg from "pg";
import { registerQuizRoutes } from "./routes/quiz.js";
import { registerResultRoutes } from "./routes/result.js";
import { registerFinishRoutes } from "./routes/finish.js";
import { registerLeaderboardRoutes } from "./routes/leaderboard.js";
import { registerEventRoutes } from "./routes/events.js";
import { registerStatsRoutes } from "./routes/stats.js";
import { seedLevel1Foods } from "./services/seed-data.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pool = new pg.Pool({
  connectionString: process.env.DATABASE_URL,
  max: 10,
});

const agentUrl = process.env.AGENT_URL || "http://agent:5000";

const fastify = Fastify({ logger: true });

// CORS
await fastify.register(cors, { origin: true });

// Init DB schema
const schema = readFileSync(join(__dirname, "schema.sql"), "utf8");
await pool.query(schema);

// Seed cuisines if empty
const { rows: existingCuisines } = await pool.query("SELECT COUNT(*) FROM cuisines");
if (Number(existingCuisines[0].count) === 0) {
  const cuisines = [
    "indian", "chinese", "italian", "mexican", "american",
    "japanese", "thai", "french", "spanish", "greek",
    "turkish", "lebanese", "korean", "vietnamese", "indonesian",
    "brazilian", "german", "british", "ethiopian", "moroccan",
  ];
  for (const name of cuisines) {
    await pool.query("INSERT INTO cuisines (name) VALUES ($1) ON CONFLICT DO NOTHING", [name]);
  }
  fastify.log.info("Seeded 20 cuisines");
}

// Seed Level 1 foods (hardcoded, no AI)
await seedLevel1Foods(pool);

// Decorate with shared deps
fastify.decorate("db", pool);
fastify.decorate("agentUrl", agentUrl);

// Routes
registerEventRoutes(fastify); // Must be first so sseBroadcast is available
registerQuizRoutes(fastify);
registerResultRoutes(fastify);
registerFinishRoutes(fastify);
registerLeaderboardRoutes(fastify);
registerStatsRoutes(fastify);

const port = 3000;
try {
  await fastify.listen({ port, host: "0.0.0.0" });
  fastify.log.info(`API listening on ${port}`);
} catch (err) {
  fastify.log.error(err);
  process.exit(1);
}
