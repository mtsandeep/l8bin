const levelInfo = {
  1: {
    name: "Level One",
    subtitle: "Single Items",
    icon: "fa-apple-whole",
    color: "retro-green",
    description: "Fruits, vegetables, meat, and fish. 100g servings.",
    tip: "Common fruits range from 30-90 cal per 100g. Meat is usually 100-250 cal.",
  },
  2: {
    name: "Level Two",
    subtitle: "Simple Meals",
    icon: "fa-utensils",
    color: "retro-blue",
    description: "Burgers, pasta, sandwiches, and more. 250g servings.",
    tip: "A typical burger is 300-600 cal. Pasta dishes range from 400-800 cal.",
  },
  3: {
    name: "Level Three",
    subtitle: "Complex Meals",
    icon: "fa-fire",
    color: "retro-accent",
    description:
      "Biryani, combos, and elaborate multi-component dishes. 500g servings.",
    tip: "Complex meals can range from 500-2500+ cal depending on ingredients.",
  },
};

export default function LevelIntro({ level, onStartLevel }) {
  const info = levelInfo[level] || levelInfo[1];

  return (
    <div className="min-h-[calc(100vh-80px)] flex items-center justify-center p-8">
      <div className="max-w-5xl mx-auto w-full">
        {/* Level Badge */}
        <div className="flex justify-center mb-8 animate-scale-in">
          <div
            className={`bg-gradient-to-br ${level === 1 ? "from-green-500 to-green-700" : level === 2 ? "from-blue-500 to-blue-700" : "from-red-500 to-red-700"} border-[4px] border-retro-ink rounded-3xl px-6 py-4 shadow-neubrutalism-lg inline-flex items-center gap-3`}
          >
            <div className="w-14 h-14 bg-white rounded-full border-[3px] border-retro-ink flex items-center justify-center">
              <span className="font-black text-2xl text-retro-accent">
                {level}
              </span>
            </div>
            <div className="text-white">
              <div className="font-mono text-sm uppercase tracking-widest opacity-90">
                {info.subtitle}
              </div>
              <div className="font-black text-2xl uppercase tracking-tight">
                {info.name}
              </div>
            </div>
          </div>
        </div>

        {/* Main Title */}
        <div
          className="text-center mb-12 animate-fade-in-up"
          style={{ animationDelay: "0.1s" }}
        >
          <h1 className="text-6xl lg:text-8xl font-black tracking-tighter leading-[0.9] mb-6 uppercase">
            Ready To
            <br />
            <span className="text-retro-accent relative inline-block">
              Begin?
            </span>
          </h1>
          <p className="text-xl font-medium text-retro-text font-mono max-w-2xl mx-auto">
            {info.description}
          </p>
        </div>

        {/* Questions Info */}
        <div
          className="mb-12 animate-fade-in-up"
          style={{ animationDelay: "0.2s" }}
        >
          <div className="bg-white border-[4px] border-retro-ink rounded-2xl px-8 py-4 shadow-neubrutalism-lg flex items-center gap-4 max-w-fit mx-auto">
            <div className="w-14 h-14 bg-retro-blue border-[3px] border-retro-ink rounded-full flex items-center justify-center">
              <i className="fa-solid fa-list-check text-white text-2xl"></i>
            </div>
            <div>
              <div className="font-mono text-sm uppercase tracking-widest text-retro-muted">
                Questions
              </div>
              <div className="font-black text-2xl sm:text-4xl uppercase text-retro-ink">
                3 Questions
              </div>
            </div>
          </div>
        </div>

        {/* Start Button */}
        <div
          className="text-center animate-fade-in-up mb-12"
          style={{ animationDelay: "0.3s" }}
        >
          <button
            onClick={onStartLevel}
            className="bg-retro-accent hover:bg-[#e03a2e] text-white border-[4px] border-retro-ink rounded-2xl py-4 sm:py-6 px-8 sm:px-16 font-mono font-black text-xl sm:text-3xl uppercase tracking-wider shadow-neubrutalism-xl transition-all active:translate-y-2 active:shadow-neubrutalism inline-flex items-center gap-2 sm:gap-4 pulse-glow"
          >
            <i className="fa-solid fa-play"></i>
            Start Level {level}
            <i className="fa-solid fa-arrow-right"></i>
          </button>

          <p className="font-mono text-sm text-retro-text mt-6">
            <i className="fa-solid fa-lightbulb mr-2"></i> {info.tip}
          </p>
        </div>

        {/* Rules Section — only show on level 1 */}
        {level === 1 && (
          <div
            className="bg-white border-[4px] border-retro-ink rounded-2xl p-8 shadow-neubrutalism-xl mb-12 animate-fade-in-up"
            style={{ animationDelay: "0.4s" }}
          >
            <div className="flex items-center gap-3 mb-6">
              <div className="w-12 h-12 bg-retro-accent border-[3px] border-retro-ink rounded-lg flex items-center justify-center">
                <i className="fa-solid fa-book text-white text-xl"></i>
              </div>
              <h2 className="font-black text-3xl uppercase">Rules</h2>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
              <div className="flex gap-4">
                <div className="flex-shrink-0">
                  <div className="w-10 h-10 bg-retro-blue border-[3px] border-retro-ink rounded-full flex items-center justify-center">
                    <i className="fa-solid fa-clock text-white"></i>
                  </div>
                </div>
                <div>
                  <h3 className="font-black text-lg mb-1 uppercase">
                    15 Seconds
                  </h3>
                  <p className="font-mono text-sm text-retro-text">
                    Timer starts immediately. Answer before it runs out.
                  </p>
                </div>
              </div>

              <div className="flex gap-4">
                <div className="flex-shrink-0">
                  <div className="w-10 h-10 bg-retro-green border-[3px] border-retro-ink rounded-full flex items-center justify-center">
                    <i className="fa-solid fa-bullseye text-white"></i>
                  </div>
                </div>
                <div>
                  <h3 className="font-black text-lg mb-1 uppercase">
                    Accuracy Matters
                  </h3>
                  <p className="font-mono text-sm text-retro-text">
                    Closer guess = more points. Average within 30% to advance.
                    Max penalty per question: 50%.
                  </p>
                </div>
              </div>

              <div className="flex gap-4">
                <div className="flex-shrink-0">
                  <div className="w-10 h-10 bg-retro-yellow border-[3px] border-retro-ink rounded-full flex items-center justify-center">
                    <i className="fa-solid fa-bolt text-white"></i>
                  </div>
                </div>
                <div>
                  <h3 className="font-black text-lg mb-1 uppercase">
                    Speed Bonus
                  </h3>
                  <p className="font-mono text-sm text-retro-text">
                    Under 5s: +500 pts. Under 10s: +200 pts.
                  </p>
                </div>
              </div>

              <div className="flex gap-4">
                <div className="flex-shrink-0">
                  <div className="w-10 h-10 bg-retro-accent border-[3px] border-retro-ink rounded-full flex items-center justify-center">
                    <i className="fa-solid fa-lock text-white"></i>
                  </div>
                </div>
                <div>
                  <h3 className="font-black text-lg mb-1 uppercase">
                    No External Help
                  </h3>
                  <p className="font-mono text-sm text-retro-text">
                    No searching or calculators. Use your nutrition knowledge.
                  </p>
                </div>
              </div>
            </div>
          </div>
        )}

        {/* Scoring Breakdown — only show on level 1 */}
        {level === 1 && (
          <div
            className="bg-retro-gray border-[4px] border-retro-ink rounded-2xl p-8 shadow-neubrutalism-lg mb-12 animate-fade-in-up"
            style={{ animationDelay: "0.5s" }}
          >
            <h2 className="font-black text-2xl uppercase mb-6 flex items-center gap-3">
              <i className="fa-solid fa-calculator text-retro-blue"></i> Scoring
            </h2>

            <div className="space-y-3">
              {[
                { label: "Exact Match", color: "green", pts: "1000" },
                { label: "Within 5%", color: "blue", pts: "800" },
                { label: "Within 10%", color: "yellow", pts: "600" },
                { label: "Within 20%", color: "orange", pts: "400" },
                { label: "Within 30%", color: "red", pts: "200" },
                { label: "Beyond 30%", color: "gray", pts: "0" },
              ].map((tier) => (
                <div
                  key={tier.label}
                  className="flex items-center justify-between p-3 bg-white border-[3px] border-retro-ink rounded-xl"
                >
                  <div className="flex items-center gap-3">
                    <div
                      className={`w-8 h-8 bg-${tier.color}-500 border-[2px] border-retro-ink rounded-full flex items-center justify-center`}
                    >
                      <i
                        className={`fa-solid ${tier.color === "green" ? "fa-check" : tier.color === "gray" ? "fa-xmark" : "fa-bullseye"} text-white text-sm`}
                      ></i>
                    </div>
                    <span className="font-bold">{tier.label}</span>
                  </div>
                  <span
                    className={`font-black font-mono text-xl text-${tier.color}-600`}
                  >
                    {tier.pts} pts
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
