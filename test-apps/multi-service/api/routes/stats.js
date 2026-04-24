export async function registerStatsRoutes(fastify) {
  fastify.get("/api/stats", async (req, reply) => {
    const { db } = fastify;

    // Count total games played (unique leaderboard entries)
    const { rows: totalGames } = await db.query(
      "SELECT COUNT(*) as count FROM leaderboard"
    );

    // Count total quiz sessions (including incomplete games)
    const { rows: totalSessions } = await db.query(
      "SELECT COUNT(*) as count FROM quiz_sessions"
    );

    // Count total foods
    const { rows: totalFoods } = await db.query(
      "SELECT COUNT(*) as count FROM foods"
    );

    // Count total cuisines
    const { rows: totalCuisines } = await db.query(
      "SELECT COUNT(*) as count FROM cuisines"
    );

    return {
      totalGamesPlayed: Number(totalGames[0].count),
      totalSessions: Number(totalSessions[0].count),
      totalFoods: Number(totalFoods[0].count),
      totalCuisines: Number(totalCuisines[0].count),
    };
  });
}
