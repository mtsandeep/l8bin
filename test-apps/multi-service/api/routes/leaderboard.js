export async function registerLeaderboardRoutes(fastify) {
  fastify.get("/api/leaderboard", async (req, reply) => {
    const { db } = fastify;
    const { rows } = await db.query(
      "SELECT name, score, title, levels_completed, level_scores, created_at FROM leaderboard ORDER BY score DESC LIMIT 10"
    );
    return rows;
  });
}
