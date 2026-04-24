import { useState } from "react";

const adjectives = [
  "Spicy",
  "Lazy",
  "Savage",
  "Mighty",
  "Cosmic",
  "Crunchy",
  "Sizzling",
  "Bold",
];
const foods = [
  "Samosa",
  "Ramen",
  "Burger",
  "Taco",
  "Sushi",
  "Pizza",
  "Pasta",
  "Noodle",
];

function generateName() {
  const adj = adjectives[Math.floor(Math.random() * adjectives.length)];
  const food = foods[Math.floor(Math.random() * foods.length)];
  const num = Math.floor(Math.random() * 100);
  return `${adj} ${food} ${num}`;
}

export default function StartScreen({ onStart }) {
  const [name, setName] = useState(generateName);

  return (
    <div className="p-8 lg:p-16 flex flex-col justify-center min-h-[calc(100vh-80px)]">
      <div className="max-w-3xl mx-auto w-full">
        {/* Hero */}
        <div className="mb-12 animate-fade-in-up">
          <h1 className="text-5xl lg:text-7xl font-black tracking-tighter leading-[0.9] mb-6 uppercase">
            Guess The <br />
            <span className="text-retro-accent relative inline-block">
              Calories.
            </span>
          </h1>
          <p className="text-lg lg:text-xl font-medium text-retro-text leading-relaxed max-w-2xl font-mono">
            Test your calorie intuition across 3 levels: single items, simple
            meals, and complex dishes.
          </p>
        </div>

        {/* Name + Start */}
        <div
          className="bg-white border-[3px] border-retro-ink rounded-2xl p-4 sm:p-6 shadow-neubrutalism-lg animate-fade-in-up"
          style={{ animationDelay: "0.2s" }}
        >
          <div className="space-y-4 sm:space-y-6">
            <div>
              <label className="block font-mono font-bold text-xs sm:text-sm uppercase mb-2">
                Enter Player Name (Optional)
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="E.g. SnackMaster99"
                className="w-full bg-retro-bg border-[3px] border-retro-ink rounded-lg px-3 py-3 sm:px-4 sm:py-4 font-mono text-base sm:text-lg focus:outline-none focus:ring-4 focus:ring-retro-blue/20 transition-all placeholder:text-retro-muted font-bold"
              />
              <p className="text-xs sm:text-sm text-retro-text mt-2 font-mono">
                <i className="fa-solid fa-circle-info mr-1"></i> Used for the
                global leaderboard.
              </p>
            </div>

            <button
              onClick={() => onStart(name || generateName())}
              className="w-full bg-retro-accent hover:bg-[#e03a2e] text-white border-[3px] border-retro-ink rounded-xl py-3 sm:py-5 px-4 sm:px-8 font-mono font-black text-lg sm:text-2xl uppercase tracking-wider shadow-neubrutalism transition-transform active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-3"
            >
              <i className="fa-solid fa-play"></i> Start Quiz
            </button>
          </div>
        </div>

        {/* Features */}
        <div className="grid grid-cols-1 md:grid-cols-3 gap-6 mt-16">
          <div
            className="border-[3px] border-retro-ink rounded-xl p-5 bg-retro-gray shadow-neubrutalism-sm animate-fade-in-up"
            style={{ animationDelay: "0.4s" }}
          >
            <i className="fa-solid fa-stopwatch text-3xl mb-3"></i>
            <h3 className="font-black text-lg uppercase mb-1">15 Seconds</h3>
            <p className="font-mono text-sm text-retro-text">
              Per question. Think fast.
            </p>
          </div>
          <div
            className="border-[3px] border-retro-ink rounded-xl p-5 bg-retro-gray shadow-neubrutalism-sm animate-fade-in-up"
            style={{ animationDelay: "0.5s" }}
          >
            <i className="fa-solid fa-layer-group text-3xl mb-3"></i>
            <h3 className="font-black text-lg uppercase mb-1">3 Levels</h3>
            <p className="font-mono text-sm text-retro-text">
              Items, meals, combos. 9 questions.
            </p>
          </div>
          <div
            className="border-[3px] border-retro-ink rounded-xl p-5 bg-retro-gray shadow-neubrutalism-sm animate-fade-in-up"
            style={{ animationDelay: "0.6s" }}
          >
            <i className="fa-solid fa-trophy text-3xl mb-3"></i>
            <h3 className="font-black text-lg uppercase mb-1">Global Rank</h3>
            <p className="font-mono text-sm text-retro-text">
              Compete worldwide.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
