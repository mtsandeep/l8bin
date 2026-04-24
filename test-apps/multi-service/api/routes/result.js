import { getPoints, checkLevelPass, getTitle } from "../services/scoring.js";

export async function registerResultRoutes(fastify) {
  fastify.post("/api/result", async (req, reply) => {
    const { db } = fastify;
    const { sessionId, answers, level = 1 } = req.body;

    if (!sessionId || !answers) {
      return reply.code(400).send({ error: "Missing sessionId or answers" });
    }

    let levelScore = 0;
    const correctCalories = {};
    const breakdown = [];

    for (const answer of answers) {
      const { rows } = await db.query(
        "SELECT calories FROM food_macros WHERE food_id = $1",
        [answer.foodId],
      );
      if (rows.length === 0) continue;

      const correct = rows[0].calories;
      correctCalories[answer.foodId] = correct;
      const points = getPoints(answer.guessed, correct, answer.timeTaken || 0);
      levelScore += points;

      // Get food name for breakdown
      const { rows: foodRows } = await db.query(
        "SELECT name FROM foods WHERE id = $1",
        [answer.foodId],
      );

      const diff = Math.abs(answer.guessed - correct);
      const diffPct = correct > 0 ? (diff / correct) * 100 : 0;

      breakdown.push({
        foodId: answer.foodId,
        foodName: foodRows[0]?.name || "Unknown",
        correctCalories: correct,
        userGuess: answer.guessed,
        difference: diff,
        differencePct: Math.round(diffPct),
        points: points,
        timeTaken: answer.timeTaken || 0,
      });

      await db.query(
        `INSERT INTO quiz_answers (session_id, food_id, selected, correct, points, level, time_taken)
         VALUES ($1, $2, $3, $4, $5, $6, $7)`,
        [sessionId, answer.foodId, answer.guessed, correct, points, level, answer.timeTaken ?? 0],
      );
    }

    const passed = checkLevelPass(answers, correctCalories);

    // Update session level_scores
    const { rows: session } = await db.query(
      "SELECT level_scores, score FROM quiz_sessions WHERE id = $1",
      [sessionId],
    );
    // pg returns JSONB as parsed JS object
    const existingScores = session[0]?.level_scores;
    const levelScores = Array.isArray(existingScores) ? existingScores : [];
    levelScores.push({ level, score: levelScore, passed });
    const newTotal = levelScores.reduce((sum, ls) => sum + ls.score, 0);

    await db.query(
      `UPDATE quiz_sessions SET score = $1, level_scores = $2::jsonb, levels_completed = $3 WHERE id = $4`,
      [newTotal, JSON.stringify(levelScores), level, sessionId],
    );

    // Save to leaderboard (UPSERT) to preserve progress
    const { rows: sessionRow } = await db.query(
      "SELECT name FROM quiz_sessions WHERE id = $1",
      [sessionId],
    );
    const playerName = sessionRow[0]?.name;

    if (playerName) {
      const title = getTitle(newTotal);
      // Check if player already exists in leaderboard
      const { rows: existing } = await db.query(
        "SELECT id FROM leaderboard WHERE name = $1",
        [playerName],
      );

      if (existing.length > 0) {
        // Update existing entry
        await db.query(
          `UPDATE leaderboard SET score = $1, title = $2, levels_completed = $3, level_scores = $4::jsonb WHERE name = $5`,
          [newTotal, title, level, JSON.stringify(levelScores), playerName],
        );
      } else {
        // Insert new entry
        await db.query(
          `INSERT INTO leaderboard (name, score, title, levels_completed, level_scores)
           VALUES ($1, $2, $3, $4, $5::jsonb)`,
          [playerName, newTotal, title, level, JSON.stringify(levelScores)],
        );
      }
    }

    return { score: levelScore, totalScore: newTotal, passed, level, levelScores, breakdown };
  });
}
