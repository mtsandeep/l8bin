# 🧠 Snack IQ — Final Build Plan

---

## 🎯 Core Idea

- Tap-based food calorie guessing game
- 3 questions per session
- All food data is **AI-generated (no seed data)**
- Database grows over time and is **bounded**

### Limits:
- 20 cuisines
- 20 foods per cuisine
- Max total = **400 foods**

👉 After reaching limits → **AI generation stops completely**

---

## 🌍 Cuisines (Insert into DB)

```

indian
chinese
italian
mexican
american
japanese
thai
french
spanish
greek
turkish
lebanese
korean
vietnamese
indonesian
brazilian
german
british
ethiopian
moroccan

````

---

## 🗄️ Database Schema

### `cuisines`
```sql
CREATE TABLE cuisines (
  id SERIAL PRIMARY KEY,
  name TEXT UNIQUE,
  target_count INT DEFAULT 20
);
````

---

### `foods`

```sql
CREATE TABLE foods (
  id SERIAL PRIMARY KEY,
  name TEXT,
  cuisine_id INT REFERENCES cuisines(id),
  items JSONB,
  created_at TIMESTAMP DEFAULT NOW(),
  UNIQUE(name, cuisine_id)
);
```

---

### `food_macros`

```sql
CREATE TABLE food_macros (
  food_id INT REFERENCES foods(id),
  calories INT,
  protein INT,
  carbs INT,
  fat INT
);
```

---

### `quiz_sessions`

```sql
CREATE TABLE quiz_sessions (
  id UUID PRIMARY KEY,
  score INT DEFAULT 0,
  created_at TIMESTAMP DEFAULT NOW()
);
```

---

### `quiz_answers`

```sql
CREATE TABLE quiz_answers (
  id SERIAL PRIMARY KEY,
  session_id UUID,
  food_id INT,
  selected INT,
  correct INT,
  points INT
);
```

---

### `leaderboard`

```sql
CREATE TABLE leaderboard (
  id SERIAL PRIMARY KEY,
  name TEXT,
  score INT,
  title TEXT,
  created_at TIMESTAMP DEFAULT NOW()
);
```

---

## 🔁 Food Generation Logic

### Step 1: Pick Cuisine (only if not full)

```sql
SELECT *
FROM cuisines c
WHERE (
  SELECT COUNT(*) FROM foods f WHERE f.cuisine_id = c.id
) < c.target_count
ORDER BY RANDOM()
LIMIT 1;
```

---

### Step 2: Fetch Existing Foods

```sql
SELECT name FROM foods WHERE cuisine_id = $1;
```

(max 19 items)

---

### Step 3: Call AI (Generate 3 foods)

#### Prompt:

```
Generate 3 realistic food dishes from {{cuisine}} cuisine.

Rules:
- Must be real, commonly known dishes
- No fusion or invented foods
- Avoid these existing dishes:
{{existing_names}}

- Do not generate very similar variants
- Prefer popular foods

Return JSON:
[
  { "name": "...", "items": ["..."] },
  { "name": "...", "items": ["..."] },
  { "name": "...", "items": ["..."] }
]
```

---

### Step 4: Filter Results

* Normalize names
* Remove duplicates (within response)
* Remove already-existing (DB check)

```js
function normalize(name) {
  return name.toLowerCase().replace(/[^a-z]/g, "");
}
```

---

### Step 5: Insert

```sql
INSERT INTO foods (name, cuisine_id, items)
VALUES (...)
ON CONFLICT (name, cuisine_id) DO NOTHING;
```

---

### Step 6: Retry Logic

* If 0 inserted → retry once
* If still 0 → skip

---

### Step 7: Stop Condition

* When all cuisines reach 20 foods → **stop AI calls permanently**

---

## ⚙️ Macro Generation

### Agent Prompt:

```
Given this meal:
{{items}}

Estimate:
- calories
- protein (g)
- carbs (g)
- fat (g)

Return realistic values.
```

---

## 🎯 Quiz Flow

### GET `/quiz`

#### Logic:

```
- create session
- ensure foods exist (trigger generation if needed, async)
- fetch 3 random foods (different cuisines)
- fetch macros
- generate options
- return questions
```

---

### Response:

```json
{
  "sessionId": "uuid",
  "questions": [
    {
      "foodId": 1,
      "name": "Ramen + Gyoza",
      "cuisine": "japanese",
      "options": [400, 700, 1100],
      "correct": 700
    }
  ]
}
```

---

## 🎯 Options Logic

```js
function generateOptions(correct) {
  return shuffle([
    correct,
    Math.round(correct * 0.7),
    Math.round(correct * 1.3)
  ]);
}
```

---

## 🧮 Scoring Logic

```js
function getPoints(selected, correct) {
  const diff = Math.abs(selected - correct) / correct;

  if (diff === 0) return 100;
  if (diff <= 0.1) return 70;
  if (diff <= 0.2) return 40;
  return 0;
}
```

---

## 📤 POST `/result`

### Request:

```json
{
  "sessionId": "...",
  "answers": [
    { "foodId": 1, "selected": 700 }
  ],
  "name": "Spicy Ramen 42"
}
```

---

### Response:

```json
{
  "score": 210,
  "title": "Snack Analyst"
}
```

---

### Logic:

```
- calculate score
- generate title (rule-based)
- insert leaderboard
- trigger async roast (agent)
- emit SSE event
```

---

## 🏆 Leaderboard

### GET `/leaderboard`

```json
[
  {
    "name": "Spicy Ramen 42",
    "score": 210,
    "title": "Snack Analyst"
  }
]
```

---

## 🔄 Real-time Updates (SSE)

### Endpoint:

```
GET /events
```

### Event:

```
event: leaderboard_update
data: { "name": "...", "score": 210, "title": "..." }
```

---

## 🤖 Agent Service

### Endpoints:

#### 1. `/generate-food`

* input: cuisine + existing names
* output: 3 foods

#### 2. `/generate-macros`

* input: items
* output: macros

#### 3. `/roast`

* input: score
* output: short roast

---

### Roast Prompt:

```
Generate a short funny roast (1–2 lines).

Input:
Score: {{score}} out of 300

Rules:
- playful, not offensive
- casual tone
- under 20 words
```

---

## ⚡ Async Behavior (Important)

```
POST /result:
  → save immediately
  → respond immediately

BACKGROUND:
  → call agent
  → update data
  → push SSE event
```

👉 Never block user on AI

---

## 🖥️ Frontend Pages

### 1. Quiz Page

* 3 questions
* tap answers

---

### 2. Result Page

* score
* editable name
* submit

---

### 3. Leaderboard Page

* list
* live updates via SSE

---

## 🧾 Name Generator (Frontend)

```js
const adjectives = ["Spicy", "Lazy", "Savage"];
const foods = ["Samosa", "Ramen", "Burger"];

function generateName() {
  return `${rand(adjectives)} ${rand(foods)} ${Math.floor(Math.random()*100)}`;
}
```

---

## 📦 Folder Structure

### API

```
/api
  routes/
    quiz.js
    result.js
    leaderboard.js
    events.js
  services/
    food.js
    scoring.js
    agent.js
```

---

### Agent

```
/agent
  routes/
    generate-food.js
    generate-macros.js
    roast.js
```

---

### Frontend

```
/src
  pages/
  components/
  services/api.js
```

---

## 💯 Final System Behavior

* Starts with empty DB
* First users trigger AI generation
* DB fills gradually (max 400 foods)
* Eventually becomes fully DB-driven
* AI stops automatically

---

## 🚀 Final Instruction

> Keep everything minimal. No auth. No overengineering. Focus on working flow and multi-service interaction.
