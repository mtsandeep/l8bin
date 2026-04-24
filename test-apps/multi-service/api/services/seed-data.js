// Hardcoded Level 1 food items with accurate macros (per 100g serving)
// Categories: fruit, vegetable, meat, fish

export const LEVEL_1_FOODS = [
  // Fruits
  { name: "Apple", category: "fruit", items: ["apple"], calories: 52, protein: 0, carbs: 14, fat: 0 },
  { name: "Banana", category: "fruit", items: ["banana"], calories: 89, protein: 1, carbs: 23, fat: 0 },
  { name: "Orange", category: "fruit", items: ["orange"], calories: 47, protein: 1, carbs: 12, fat: 0 },
  { name: "Grapes", category: "fruit", items: ["grapes"], calories: 69, protein: 1, carbs: 18, fat: 0 },
  { name: "Mango", category: "fruit", items: ["mango"], calories: 60, protein: 1, carbs: 15, fat: 0 },
  { name: "Strawberries", category: "fruit", items: ["strawberries"], calories: 32, protein: 1, carbs: 8, fat: 0 },
  { name: "Watermelon", category: "fruit", items: ["watermelon"], calories: 30, protein: 1, carbs: 8, fat: 0 },
  { name: "Avocado", category: "fruit", items: ["avocado"], calories: 160, protein: 2, carbs: 9, fat: 15 },

  // Vegetables
  { name: "Broccoli", category: "vegetable", items: ["broccoli"], calories: 34, protein: 3, carbs: 7, fat: 0 },
  { name: "Carrot", category: "vegetable", items: ["carrot"], calories: 41, protein: 1, carbs: 10, fat: 0 },
  { name: "Potato", category: "vegetable", items: ["potato"], calories: 77, protein: 2, carbs: 17, fat: 0 },
  { name: "Tomato", category: "vegetable", items: ["tomato"], calories: 18, protein: 1, carbs: 4, fat: 0 },
  { name: "Spinach", category: "vegetable", items: ["spinach"], calories: 23, protein: 3, carbs: 4, fat: 0 },
  { name: "Sweet Potato", category: "vegetable", items: ["sweet potato"], calories: 86, protein: 2, carbs: 20, fat: 0 },
  { name: "Bell Pepper", category: "vegetable", items: ["bell pepper"], calories: 31, protein: 1, carbs: 6, fat: 0 },
  { name: "Onion", category: "vegetable", items: ["onion"], calories: 40, protein: 1, carbs: 9, fat: 0 },

  // Meat
  { name: "Chicken Breast", category: "meat", items: ["chicken breast"], calories: 165, protein: 31, carbs: 0, fat: 4 },
  { name: "Egg", category: "meat", items: ["egg"], calories: 155, protein: 13, carbs: 1, fat: 11 },
  { name: "Beef Steak", category: "meat", items: ["beef steak"], calories: 250, protein: 26, carbs: 0, fat: 16 },
  { name: "Pork Chop", category: "meat", items: ["pork chop"], calories: 242, protein: 27, carbs: 0, fat: 14 },
  { name: "Turkey Breast", category: "meat", items: ["turkey breast"], calories: 189, protein: 29, carbs: 0, fat: 7 },
  { name: "Tofu", category: "meat", items: ["tofu"], calories: 76, protein: 8, carbs: 2, fat: 5 },
  { name: "Shrimp", category: "meat", items: ["shrimp"], calories: 99, protein: 24, carbs: 0, fat: 0 },
  { name: "Lamb Chop", category: "meat", items: ["lamb chop"], calories: 282, protein: 26, carbs: 0, fat: 20 },

  // Fish
  { name: "Salmon", category: "fish", items: ["salmon"], calories: 208, protein: 20, carbs: 0, fat: 13 },
  { name: "Cod", category: "fish", items: ["cod"], calories: 82, protein: 18, carbs: 0, fat: 1 },
  { name: "Tuna", category: "fish", items: ["tuna"], calories: 132, protein: 28, carbs: 0, fat: 1 },
  { name: "Sardines", category: "fish", items: ["sardines"], calories: 208, protein: 25, carbs: 0, fat: 11 },
  { name: "Tilapia", category: "fish", items: ["tilapia"], calories: 96, protein: 20, carbs: 0, fat: 2 },
];

export async function seedLevel1Foods(db) {
  // Migration: Delete old orphan foods from before the level system.
  // These have cuisine_id but no category, and were generated with unknown serving sizes.
  // Must delete in order: quiz_answers → food_macros → foods (FK constraints).
  const orphanFoodIds = await db.query(
    "SELECT id FROM foods WHERE cuisine_id IS NOT NULL AND category IS NULL"
  );
  if (orphanFoodIds.rows.length > 0) {
    const ids = orphanFoodIds.rows.map((r) => r.id);
    await db.query("DELETE FROM quiz_answers WHERE food_id = ANY($1)", [ids]);
    await db.query("DELETE FROM food_macros WHERE food_id = ANY($1)", [ids]);
    await db.query("DELETE FROM foods WHERE id = ANY($1)", [ids]);
  }

  // Check if level 1 seed foods (with category) already exist
  const { rows } = await db.query(
    "SELECT COUNT(*) FROM foods WHERE level = 1 AND category IS NOT NULL"
  );
  if (Number(rows[0].count) > 0) return;

  for (const food of LEVEL_1_FOODS) {
    const { rows: inserted } = await db.query(
      `INSERT INTO foods (name, items, level, serving_size, category)
       VALUES ($1, $2, 1, 100, $3)
       ON CONFLICT DO NOTHING
       RETURNING id`,
      [food.name, JSON.stringify(food.items), food.category]
    );

    if (inserted.length > 0) {
      await db.query(
        `INSERT INTO food_macros (food_id, calories, protein, carbs, fat)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING`,
        [inserted[0].id, food.calories, food.protein, food.carbs, food.fat]
      );
    }
  }
}
