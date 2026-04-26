CREATE TABLE IF NOT EXISTS cuisines (
  id SERIAL PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  target_count INT DEFAULT 20
);

CREATE TABLE IF NOT EXISTS foods (
  id SERIAL PRIMARY KEY,
  name TEXT NOT NULL,
  cuisine_id INT REFERENCES cuisines(id),
  items JSONB DEFAULT '[]'::jsonb,
  level INT DEFAULT 1,
  serving_size INT DEFAULT 100,
  category TEXT,
  top_ingredients JSONB DEFAULT '[]'::jsonb,
  created_at TIMESTAMP DEFAULT NOW(),
  UNIQUE(name, cuisine_id)
);

CREATE TABLE IF NOT EXISTS food_macros (
  food_id INT PRIMARY KEY REFERENCES foods(id),
  calories INT NOT NULL,
  protein INT NOT NULL,
  carbs INT NOT NULL,
  fat INT NOT NULL
);

CREATE TABLE IF NOT EXISTS quiz_sessions (
  id UUID PRIMARY KEY,
  name TEXT,
  score INT DEFAULT 0,
  levels_completed INT DEFAULT 0,
  level_scores JSONB DEFAULT '[]'::jsonb,
  created_at TIMESTAMP DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS quiz_answers (
  id SERIAL PRIMARY KEY,
  session_id UUID REFERENCES quiz_sessions(id),
  food_id INT REFERENCES foods(id),
  selected INT NOT NULL,
  correct INT NOT NULL,
  points INT NOT NULL,
  level INT DEFAULT 1,
  time_taken NUMERIC(5,2) DEFAULT 0
);

CREATE TABLE IF NOT EXISTS leaderboard (
  id SERIAL PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  score INT NOT NULL,
  title TEXT,
  levels_completed INT DEFAULT 1,
  level_scores JSONB DEFAULT '[]'::jsonb,
  created_at TIMESTAMP DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS quiz_request_counts (
  level INT PRIMARY KEY,
  request_count INT DEFAULT 0
);

-- Indexes for read performance
CREATE INDEX IF NOT EXISTS idx_foods_cuisine_level ON foods(cuisine_id, level);
CREATE INDEX IF NOT EXISTS idx_foods_level ON foods(level);
CREATE INDEX IF NOT EXISTS idx_foods_level_category ON foods(level, category);
CREATE INDEX IF NOT EXISTS idx_quiz_answers_session_id ON quiz_answers(session_id);
