import { v4 as uuid } from "uuid";
import { callAgentGenerateFood } from "../services/food.js";

export async function registerQuizRoutes(fastify) {
  fastify.get("/api/quiz", async (req, reply) => {
    const { db, agentUrl } = fastify;
    const { level = 1, sessionId, name } = req.query;

    const parsedLevel = Math.max(1, Math.min(3, parseInt(level) || 1));

    // Level 1 uses hardcoded seed data, no AI generation needed
    if (parsedLevel === 1) {
      const { rows } = await db.query(`
        SELECT f.id, f.name, f.category, f.serving_size
        FROM food_macros fm
        JOIN foods f ON fm.food_id = f.id
        WHERE f.level = 1 AND f.category IS NOT NULL
        ORDER BY RANDOM()
        LIMIT 3
      `);

      if (rows.length < 3) {
        return reply.code(503).send({ error: "Not enough foods yet" });
      }

      const sid = sessionId || uuid();
      if (!sessionId) {
        await db.query("INSERT INTO quiz_sessions (id, name) VALUES ($1, $2)", [sid, name || null]);
      } else if (name) {
        await db.query("UPDATE quiz_sessions SET name = $1 WHERE id = $2", [name, sid]);
      }

      const questions = rows.map((f) => ({
        foodId: f.id,
        name: f.name,
        category: f.category,
        servingSize: f.serving_size,
        sliderRange: { min: 0, max: 400, step: 5 },
      }));

      return { sessionId: sid, level: parsedLevel, questions };
    }

    // Levels 2-3: Get random cuisine and its food count in one query
    const { rows: cuisineResult } = await db.query(`
      WITH random_cuisine AS (
        SELECT * FROM cuisines ORDER BY RANDOM() LIMIT 1
      )
      SELECT
        rc.*,
        COALESCE(
          (SELECT COUNT(*) FROM foods f
           JOIN food_macros fm ON fm.food_id = f.id
           WHERE f.cuisine_id = rc.id AND f.level = $1),
          0
        ) as food_count
      FROM random_cuisine rc
    `, [parsedLevel]);

    const cuisine = cuisineResult[0];
    if (!cuisine) {
      return reply.code(503).send({ error: "No cuisines available" });
    }

    const count = Number(cuisine.food_count);

    // Get request count for this level
    const { rows: countRow } = await db.query(
      "SELECT request_count FROM quiz_request_counts WHERE level = $1",
      [parsedLevel]
    );
    const requestCount = Number(countRow[0]?.request_count || 0);

    // Increment request count
    await db.query(
      `INSERT INTO quiz_request_counts (level, request_count) VALUES ($1, 1)
       ON CONFLICT (level) DO UPDATE SET request_count = quiz_request_counts.request_count + 1`,
      [parsedLevel]
    );

    // If cuisine has < 9 foods, always call AI
    // If 9-20 foods, only call AI on 5th request
    const needsAi = count < 9 || (count >= 9 && count < 20 && requestCount % 5 === 0);

    if (!needsAi) {
      // Return from DB
      const { rows: dbFoods } = await db.query(`
        SELECT f.id, f.name, f.cuisine_id, c.name AS cuisine, f.serving_size, f.top_ingredients
        FROM food_macros fm
        JOIN foods f ON fm.food_id = f.id
        JOIN cuisines c ON f.cuisine_id = c.id
        WHERE f.level = $1
        ORDER BY RANDOM()
        LIMIT 3
      `, [parsedLevel]);

      const sid = sessionId || uuid();
      if (!sessionId) {
        await db.query("INSERT INTO quiz_sessions (id, name) VALUES ($1, $2)", [sid, name || null]);
      } else if (name) {
        await db.query("UPDATE quiz_sessions SET name = $1 WHERE id = $2", [name, sid]);
      }

      const sliderMax = parsedLevel === 2 ? 1000 : 3000;
      const questions = dbFoods.map((f) => ({
        foodId: f.id,
        name: f.name,
        cuisine: f.cuisine,
        servingSize: f.serving_size,
        topIngredients: f.top_ingredients || [],
        sliderRange: { min: 0, max: sliderMax, step: 10 },
      }));

      return { sessionId: sid, level: parsedLevel, questions };
    }

    // Not enough foods in DB - call AI directly
    const servingSize = parsedLevel === 3 ? 500 : 250;
    const foods = await callAgentGenerateFood(agentUrl, cuisine.name, [], parsedLevel, servingSize);

    // Insert into DB synchronously to get real IDs (batch insert)
    const foodValues = foods.map((food) => {
      const name = food.name.trim();
      const items = food.items || [];
      const topIngredients = (food.top_ingredients || []).map((ing) => ({
        name: ing.name,
        weight: Math.round((ing.percentage / 100) * servingSize),
      }));
      return `('${name.replace(/'/g, "''")}', ${cuisine.id}, '${JSON.stringify(items).replace(/'/g, "''")}'::jsonb, ${parsedLevel}, ${servingSize}, '${JSON.stringify(topIngredients).replace(/'/g, "''")}'::jsonb)`;
    }).join(', ');

    const { rows: inserted } = await db.query(
      `INSERT INTO foods (name, cuisine_id, items, level, serving_size, top_ingredients)
       VALUES ${foodValues}
       ON CONFLICT (name, cuisine_id) DO UPDATE SET
         items = EXCLUDED.items,
         top_ingredients = EXCLUDED.top_ingredients
       RETURNING id, name`
    );

    const insertedIds = inserted.map(r => r.id);
    const foodIdMap = Object.fromEntries(inserted.map(r => [r.name, r.id]));

    // Batch insert macros
    const macroValues = foods.map((food) => {
      const foodId = foodIdMap[food.name.trim()];
      return `(${foodId}, ${food.calories}, ${food.protein}, ${food.carbs}, ${food.fat})`;
    }).join(', ');

    await db.query(
      `INSERT INTO food_macros (food_id, calories, protein, carbs, fat)
       VALUES ${macroValues}
       ON CONFLICT (food_id) DO UPDATE SET
         calories = EXCLUDED.calories,
         protein = EXCLUDED.protein,
         carbs = EXCLUDED.carbs,
         fat = EXCLUDED.fat`
    );

    // If we don't have 3 foods from AI, fill remaining from DB
    if (insertedIds.length < 3) {
      const { rows: additionalFoods } = await db.query(`
        SELECT f.id, f.name, f.cuisine_id, c.name AS cuisine, f.serving_size, f.top_ingredients
        FROM food_macros fm
        JOIN foods f ON fm.food_id = f.id
        JOIN cuisines c ON f.cuisine_id = c.id
        WHERE f.level = $1 AND f.id NOT IN (${insertedIds.join(',')})
        ORDER BY RANDOM()
        LIMIT ${3 - insertedIds.length}
      `, [parsedLevel]);

      additionalFoods.forEach(f => {
        insertedIds.push(f.id);
        foods.push({
          name: f.name,
          top_ingredients: f.top_ingredients || [],
        });
      });
    }

    // If still don't have 3 foods, throw error
    if (insertedIds.length < 3) {
      return reply.code(503).send({ error: "Not enough foods yet" });
    }

    const sid = sessionId || uuid();
    if (!sessionId) {
      await db.query("INSERT INTO quiz_sessions (id, name) VALUES ($1, $2)", [sid, name || null]);
    } else if (name) {
      await db.query("UPDATE quiz_sessions SET name = $1 WHERE id = $2", [name, sid]);
    }

    const sliderMax = parsedLevel === 2 ? 1000 : 3000;
    const questions = foods.map((f, i) => ({
      foodId: insertedIds[i],
      name: f.name,
      cuisine: cuisine.name,
      servingSize: servingSize,
      topIngredients: f.top_ingredients || [],
      sliderRange: { min: 0, max: sliderMax, step: 10 },
    }));

    return { sessionId: sid, level: parsedLevel, questions };
  });
}
