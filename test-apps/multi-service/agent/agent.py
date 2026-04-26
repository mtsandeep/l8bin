import json
import os

from bottle import Bottle, request, response
from openai import OpenAI

app = Bottle()

OPENROUTER_API_KEY = os.environ.get("OPENROUTER_API_KEY", "")

if not OPENROUTER_API_KEY:
    print("WARNING: OPENROUTER_API_KEY is not set — AI endpoints will fail")

client = OpenAI(
    base_url="https://openrouter.ai/api/v1",
    api_key=OPENROUTER_API_KEY,
)


def _call(messages):
    try:
        resp = client.chat.completions.create(
            model="google/gemini-3.1-flash-lite-preview",
            messages=messages,
            temperature=0.8,
        )
        return resp.choices[0].message.content.strip()
    except Exception as e:
        raise RuntimeError(f"OpenAI call failed: {e}")


@app.route("/health")
def health():
    response.content_type = "application/json"
    return json.dumps({"status": "ok"})


@app.error(502)
def error502(error):
    response.content_type = "application/json"
    return json.dumps({"error": str(error)})


def _json_response(data, status=200):
    response.content_type = "application/json"
    response.status = status
    return json.dumps(data)


@app.post("/generate-food")
def generate_food():
    body = request.json
    cuisine = body["cuisine"]
    existing = body.get("existing", [])
    level = body.get("level", 2)
    serving_size = body.get("serving_size", 250)

    existing_str = "\n".join(f"- {n}" for n in existing) if existing else "None"

    level_prompts = {
        2: "simple meal dishes (e.g., burger, pasta, chicken fry, sandwich). These are commonly prepared single-dish meals.",
        3: "complex multi-ingredient meal dishes (e.g., chicken biryani, meal combos, elaborate dishes with rice, sides and sauces).",
    }

    level_instruction = level_prompts.get(level, level_prompts[2])

    messages = [
        {
            "role": "system",
            "content": (
                "You generate realistic food dishes with nutritional macros for a calorie guessing game.\n"
                "Return ONLY valid JSON, no markdown fences.\n"
                'Format: [{"name": "Dish Name", "items": ["ingredient1", "ingredient2"], "top_ingredients": [{"name": "ingredient1", "percentage": 40}, ...], "calories": 500, "protein": 25, "carbs": 45, "fat": 18}, ...]\n'
                "Generate exactly 3 dishes. Keep names concise.\n"
                "Include top_ingredients with the 5 main ingredients and their percentage (0-100) of the total dish.\n"
                "IMPORTANT: The percentages must add up to approximately 100% total.\n"
                f"Estimate macros for a typical serving size of {serving_size}g."
            ),
        },
        {
            "role": "user",
            "content": (
                f"Generate 3 realistic {level_instruction}\n"
                f"Cuisine: {cuisine}.\n"
                f"Must be real, commonly known dishes.\n"
                f"No fusion or invented foods.\n"
                f"Avoid these existing dishes:\n{existing_str}"
            ),
        },
    ]

    raw = _call(messages)
    try:
        if raw.startswith("```"):
            raw = raw.split("\n", 1)[1].rsplit("```", 1)[0]
        foods = json.loads(raw)
    except json.JSONDecodeError:
        return _json_response({"foods": []})

    # Validate response structure
    if not isinstance(foods, list):
        return _json_response({"foods": []})

    validated_foods = []
    for food in foods:
        # Check required fields
        if not isinstance(food, dict):
            continue
        if not all(key in food for key in ["name", "items", "top_ingredients", "calories", "protein", "carbs", "fat"]):
            continue

        # Validate field types
        if not isinstance(food["name"], str) or not food["name"].strip():
            continue
        if not isinstance(food["items"], list):
            continue
        if not isinstance(food["top_ingredients"], list):
            continue
        if not all(isinstance(m, (int, float)) for m in [food["calories"], food["protein"], food["carbs"], food["fat"]]):
            continue

        # Validate top_ingredients structure
        valid_ingredients = []
        for ing in food["top_ingredients"]:
            if not isinstance(ing, dict):
                continue
            if not all(key in ing for key in ["name", "percentage"]):
                continue
            if not isinstance(ing["name"], str) or not ing["name"].strip():
                continue
            if not isinstance(ing["percentage"], (int, float)):
                continue
            valid_ingredients.append(ing)

        # Check percentages add up to approximately 100%
        if valid_ingredients:
            total_pct = sum(ing["percentage"] for ing in valid_ingredients)
            if total_pct < 80 or total_pct > 120:  # Allow some margin
                continue

        validated_foods.append({
            "name": food["name"].strip(),
            "items": food["items"],
            "top_ingredients": valid_ingredients,
            "calories": int(food["calories"]),
            "protein": int(food["protein"]),
            "carbs": int(food["carbs"]),
            "fat": int(food["fat"]),
        })

    return _json_response({"foods": validated_foods})


@app.post("/roast")
def roast():
    body = request.json
    score = body["score"]

    messages = [
        {
            "role": "system",
            "content": (
                "You roast quiz players based on their score (out of 10800).\n"
                "Return ONLY the roast text, nothing else.\n"
                "Playful, not offensive. Casual tone. Under 20 words."
            ),
        },
        {"role": "user", "content": f"Roast this player. Score: {score} out of 10800."},
    ]

    raw = _call(messages)
    return _json_response({"roast": raw})


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
