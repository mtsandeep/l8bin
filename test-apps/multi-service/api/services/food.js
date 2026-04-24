// Pick a random cuisine that hasn't reached its target food count for a given level
export async function pickCuisine(db, level) {
  const { rows } = await db.query(`
    SELECT c.*
    FROM cuisines c
    WHERE (SELECT COUNT(*) FROM foods f WHERE f.cuisine_id = c.id AND f.level = $1) < c.target_count
    ORDER BY RANDOM()
    LIMIT 1
  `, [level]);
  return rows[0] || null;
}

// Get existing food names for a cuisine at a given level
export async function getExistingFoods(db, cuisineId, level) {
  const { rows } = await db.query(
    "SELECT name FROM foods WHERE cuisine_id = $1 AND level = $2",
    [cuisineId, level],
  );
  return rows.map((r) => r.name);
}

// Check if all cuisines are full for a given level
export async function isDbFull(db, level) {
  const { rows } = await db.query(`
    SELECT COUNT(*) as full_count FROM cuisines c
    WHERE (SELECT COUNT(*) FROM foods f WHERE f.cuisine_id = c.id AND f.level = $1) >= c.target_count
  `, [level]);
  const { rows: total } = await db.query("SELECT COUNT(*) FROM cuisines");
  return Number(rows[0].full_count) >= Number(total[0].count);
}

// Insert foods, skip duplicates
export async function insertFoods(db, foods, cuisineId, level = 2, servingSize = 250) {
  let inserted = 0;
  for (const food of foods) {
    const name = food.name.trim();
    const items = food.items || [];
    const topIngredients = (food.top_ingredients || []).map((ing) => ({
      name: ing.name,
      weight: Math.round((ing.percentage / 100) * servingSize),
    }));
    const { rowCount } = await db.query(
      `INSERT INTO foods (name, cuisine_id, items, level, serving_size, top_ingredients)
       VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING RETURNING id`,
      [name, cuisineId, JSON.stringify(items), level, servingSize, JSON.stringify(topIngredients)],
    );
    if (rowCount > 0) {
      inserted++;
    }
  }
  return inserted;
}

// Call agent to generate foods for a level
export async function callAgentGenerateFood(agentUrl, cuisine, existing, level = 2, servingSize = 250) {
  const res = await fetch(`${agentUrl}/generate-food`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ cuisine, existing, level, serving_size: servingSize }),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`Agent /generate-food failed (${res.status}): ${body}`);
  }
  const data = await res.json();
  return data.foods || [];
}

// Insert foods with macros (single operation, since agent now returns both)
export async function insertFoodsWithMacros(db, foods, cuisineId, level, servingSize) {
  for (const food of foods) {
    const name = food.name.trim();
    const items = food.items || [];
    const topIngredients = (food.top_ingredients || []).map((ing) => ({
      name: ing.name,
      weight: Math.round((ing.percentage / 100) * servingSize),
    }));

    const { rows: inserted } = await db.query(
      `INSERT INTO foods (name, cuisine_id, items, level, serving_size, top_ingredients)
       VALUES ($1, $2, $3, $4, $5, $6)
       ON CONFLICT (name, cuisine_id) DO UPDATE SET
         items = EXCLUDED.items,
         top_ingredients = EXCLUDED.top_ingredients
       RETURNING id`,
      [name, cuisineId, JSON.stringify(items), level, servingSize, JSON.stringify(topIngredients)]
    );

    if (inserted.length > 0) {
      const foodId = inserted[0].id;
      await db.query(
        `INSERT INTO food_macros (food_id, calories, protein, carbs, fat)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (food_id) DO UPDATE SET
           calories = EXCLUDED.calories,
           protein = EXCLUDED.protein,
           carbs = EXCLUDED.carbs,
           fat = EXCLUDED.fat`,
        [foodId, food.calories, food.protein, food.carbs, food.fat]
      );
    }
  }
}


// Call agent to roast
export async function callAgentRoast(agentUrl, score) {
  try {
    const res = await fetch(`${agentUrl}/roast`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ score }),
    });
    if (!res.ok) return null;
    const data = await res.json();
    return data.roast || null;
  } catch {
    return null;
  }
}

// Ensure enough foods exist for a quiz level, generate if needed
export async function ensureFoods(fastify, level = 1) {
  const { db, agentUrl } = fastify;

  // Level 1 uses hardcoded seed data, no AI generation needed
  if (level === 1) return;

  const servingSize = level === 3 ? 500 : 250;

  // Increment request count for this level
  await db.query(
    `INSERT INTO quiz_request_counts (level, request_count) VALUES ($1, 1)
     ON CONFLICT (level) DO UPDATE SET request_count = quiz_request_counts.request_count + 1`,
    [level]
  );

  // Get current request count
  const { rows: countRow } = await db.query(
    "SELECT request_count FROM quiz_request_counts WHERE level = $1",
    [level]
  );
  const requestCount = Number(countRow[0]?.request_count || 0);

  // Check if DB is full for this level (all cuisines have 20 items)
  if (await isDbFull(db, level)) return;

  // Pick a cuisine that needs foods for this level
  const cuisine = await pickCuisine(db, level);
  if (!cuisine) return;

  // Check food count for this specific cuisine
  const { rows: cuisineFoodCount } = await db.query(
    "SELECT COUNT(*) FROM foods f JOIN food_macros fm ON fm.food_id = f.id WHERE f.cuisine_id = $1 AND f.level = $2",
    [cuisine.id, level]
  );
  const count = Number(cuisineFoodCount[0].count);

  // Only hit AI every 5 requests, but always generate if cuisine has < 9 items
  fastify.log.info({ level, cuisine: cuisine.name, foodCount: count, requestCount }, "ensureFoods check");
  if (count >= 9 && requestCount % 5 !== 0) return;

  const existing = await getExistingFoods(db, cuisine.id, level);
  const foods = await callAgentGenerateFood(agentUrl, cuisine.name, existing, level);

  if (foods.length === 0) return;

  const inserted = await insertFoods(db, foods, cuisine.id, level, servingSize);

  // Generate macros for newly inserted foods
  if (inserted > 0) {
    const { rows: newFoods } = await db.query(
      `SELECT id, items FROM foods WHERE cuisine_id = $1 AND level = $2 ORDER BY id DESC LIMIT $3`,
      [cuisine.id, level, inserted],
    );

    for (const food of newFoods) {
      const items = food.items;
      if (!items || items.length === 0) continue;

      const macros = await callAgentGenerateMacros(agentUrl, items.join(", "), servingSize);
      const estimatedServingSize = macros.estimated_serving_size || servingSize;
      const scaleFactor = servingSize / estimatedServingSize;

      await db.query(
        `INSERT INTO food_macros (food_id, calories, protein, carbs, fat)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING`,
        [
          food.id,
          Math.round(macros.calories * scaleFactor),
          Math.round(macros.protein * scaleFactor),
          Math.round(macros.carbs * scaleFactor),
          Math.round(macros.fat * scaleFactor),
        ],
      );
    }
  }
}
