import { getTitle } from "../services/scoring.js";
import { callAgentRoast } from "../services/food.js";

export async function registerFinishRoutes(fastify) {
  fastify.post("/api/finish", async (req, reply) => {
    const { db, agentUrl } = fastify;
    const { sessionId, name } = req.body;

    if (!sessionId || !name) {
      return reply.code(400).send({ error: "Missing sessionId or name" });
    }

    const { rows: session } = await db.query(
      "SELECT score, levels_completed, level_scores FROM quiz_sessions WHERE id = $1",
      [sessionId],
    );

    if (session.length === 0) {
      return reply.code(404).send({ error: "Session not found" });
    }

    const { score: totalScore, levels_completed, level_scores } = session[0];
    const title = getTitle(totalScore);

    // Count correct answers and calculate stats
    const { rows: answers } = await db.query(
      "SELECT selected, correct, points, COALESCE(time_taken, 0) as time_taken FROM quiz_answers WHERE session_id = $1",
      [sessionId],
    );
    let correctCount = 0;
    let totalDiff = 0;
    let totalTime = 0;
    let bestQuestion = { points: 0 };
    let worstQuestion = { points: Infinity };

    for (const answer of answers) {
      const diff = Math.abs(answer.selected - answer.correct) / answer.correct;
      if (diff <= 0.20) correctCount++;
      totalDiff += diff;
      totalTime += parseFloat(answer.time_taken) || 0;

      if (answer.points > bestQuestion.points) {
        bestQuestion = { points: answer.points };
      }
      if (answer.points < worstQuestion.points) {
        worstQuestion = { points: answer.points };
      }
    }

    const avgAccuracy = answers.length > 0 ? Math.round((1 - totalDiff / answers.length) * 100) : 0;
    const avgTime = answers.length > 0 && totalTime > 0 ? totalTime / answers.length : 0;

    const { rows: lb } = await db.query(
      `INSERT INTO leaderboard (name, score, title, levels_completed, level_scores)
       VALUES ($1, $2, $3, $4, $5::jsonb)
       ON CONFLICT (name) DO UPDATE SET
         score = EXCLUDED.score,
         title = EXCLUDED.title,
         levels_completed = EXCLUDED.levels_completed,
         level_scores = EXCLUDED.level_scores
       RETURNING id`,
      [name, totalScore, title, levels_completed, JSON.stringify(level_scores)],
    );

    // Calculate rank
    const { rows: rankRows } = await db.query(
      "SELECT COUNT(*) + 1 as rank FROM leaderboard WHERE score > $1",
      [totalScore],
    );
    const rank = Number(rankRows[0].rank);

    // Fire-and-forget roast
    callAgentRoast(agentUrl, totalScore).then((roast) => {
      if (roast) {
        db.query("UPDATE leaderboard SET title = $1 WHERE id = $2", [roast, lb[0].id]);
        fastify.sseBroadcast({
          name,
          score: totalScore,
          title: roast,
          levels_completed,
          level_scores,
        });
      }
    });

    return {
      totalScore,
      title,
      rank,
      levelsCompleted: levels_completed,
      levelScores: level_scores,
      correctCount,
      avgAccuracy,
      avgTime,
    };
  });
}
