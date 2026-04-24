import { useEffect } from "react";

const levelNames = {
  1: "Single Items",
  2: "Simple Meals",
  3: "Complex Meals",
};

export default function LevelComplete({ result, level, onContinue, onQuit }) {
  const { score, passed } = result;
  const maxScore = 3600;
  const pct = Math.round((score / maxScore) * 100);

  // Confetti effect
  useEffect(() => {
    const colors = ["#FF4B3E", "#2563EB", "#F59E0B", "#10B981", "#8B5CF6"];
    const container = document.getElementById("level-complete-screen");
    if (!container) return;

    for (let i = 0; i < 40; i++) {
      setTimeout(() => {
        const confetti = document.createElement("div");
        confetti.className = "confetti-piece";
        confetti.style.left = Math.random() * 100 + "%";
        confetti.style.background =
          colors[Math.floor(Math.random() * colors.length)];
        confetti.style.animationDelay = Math.random() * 0.5 + "s";
        confetti.style.animationDuration = Math.random() * 2 + 2 + "s";
        confetti.style.width = Math.random() * 8 + 6 + "px";
        confetti.style.height = Math.random() * 8 + 6 + "px";
        if (Math.random() > 0.5) confetti.style.borderRadius = "50%";
        container.appendChild(confetti);
        setTimeout(() => confetti.remove(), 3500);
      }, i * 50);
    }
  }, []);

  const isLastLevel = level === 3;

  return (
    <div
      id="level-complete-screen"
      className="min-h-[calc(100vh-80px)] w-full flex items-center justify-center p-4 sm:p-6 md:p-8 relative overflow-hidden"
    >
      {/* Floating Background Icons */}
      <div className="absolute inset-0 pointer-events-none overflow-hidden">
        <div className="absolute top-[10%] left-[5%] text-6xl opacity-10">
          <i className="fa-solid fa-trophy text-retro-accent"></i>
        </div>
        <div className="absolute top-[20%] right-[8%] text-5xl opacity-10">
          <i className="fa-solid fa-star text-retro-blue"></i>
        </div>
        <div className="absolute bottom-[15%] left-[10%] text-7xl opacity-10">
          <i className="fa-solid fa-medal text-yellow-500"></i>
        </div>
      </div>

      <div className="relative z-10 w-full max-w-4xl">
        {/* Status Badge */}
        <div
          className="flex justify-center mb-8 animate-slide-up"
          style={{ animationDelay: "0.2s" }}
        >
          <div
            className={`inline-flex items-center gap-3 ${passed ? "bg-green-500" : "bg-retro-accent"} border-[4px] border-retro-ink rounded-full px-6 py-3 shadow-neubrutalism-lg`}
          >
            <i
              className={`fa-solid ${passed ? "fa-check-circle" : "fa-times-circle"} text-white text-2xl`}
            ></i>
            <span className="font-mono font-black text-white text-lg uppercase tracking-wider">
              {isLastLevel
                ? "All Levels Complete!"
                : passed
                  ? `Level ${level} Complete!`
                  : "Level Failed"}
            </span>
          </div>
        </div>

        {/* Main Card */}
        <div
          className="bg-white border-[6px] border-retro-ink rounded-3xl p-6 sm:p-8 md:p-12 shadow-neubrutalism-xl relative overflow-hidden animate-slide-up"
          style={{ animationDelay: "0.4s" }}
        >
          {/* Heading */}
          <div className="text-center mb-6">
            <h1 className="font-black text-5xl sm:text-6xl md:text-7xl mb-4 leading-none">
              <span className="text-retro-ink">LEVEL</span>
              <span className="text-retro-accent block mt-2">COMPLETE!</span>
            </h1>
          </div>

          {/* Stats Grid */}
          <div className="flex flex-col sm:grid sm:grid-cols-2 gap-4 mb-5">
            <div className="bg-retro-gray border-[3px] border-retro-ink rounded-xl p-4 text-center shadow-neubrutalism-sm">
              <div className="flex items-center justify-center gap-2 sm:flex-col sm:gap-0">
                <div className="flex items-center gap-2">
                  <div className="text-2xl">
                    <i className="fa-solid fa-fire text-orange-500"></i>
                  </div>
                  <div className="font-black text-2xl text-retro-ink">
                    {score}
                  </div>
                </div>
                <div className="font-mono text-sm text-retro-text uppercase sm:mt-1">
                  Points
                </div>
              </div>
            </div>
            <div className="bg-retro-gray border-[3px] border-retro-ink rounded-xl p-4 text-center shadow-neubrutalism-sm">
              <div className="flex items-center justify-center gap-2 sm:flex-col sm:gap-0">
                <div className="flex items-center gap-2">
                  <div className="text-2xl">
                    <i
                      className={`fa-solid ${passed ? "fa-check text-green-500" : "fa-xmark text-red-500"}`}
                    ></i>
                  </div>
                  <div className="font-black text-2xl text-retro-ink">
                    {passed ? "PASSED" : "FAILED"}
                  </div>
                </div>
                <div className="font-mono text-xs text-retro-text uppercase sm:mt-1">
                  {passed ? "Ready for next level" : "Avg accuracy below 20%"}
                </div>
              </div>
            </div>
          </div>

          {/* Level Info */}
          <div className="bg-retro-gray border-[3px] border-retro-ink rounded-xl p-4 mb-10">
            <div className="flex items-center justify-between">
              <div>
                <div className="font-mono text-sm text-retro-muted uppercase tracking-wider mb-1">
                  Level Type
                </div>
                <div className="font-black text-xl text-retro-ink">
                  {levelNames[level]}
                </div>
              </div>
              <div className="text-right">
                <div className="font-mono text-sm text-retro-muted uppercase tracking-wider mb-1">
                  Accuracy
                </div>
                <div
                  className={`font-black text-xl ${pct >= 60 ? "text-green-600" : pct >= 30 ? "text-yellow-600" : "text-red-600"}`}
                >
                  {pct}%
                </div>
              </div>
            </div>
          </div>

          {/* Action Buttons */}
          <div className="flex flex-col sm:flex-row gap-4">
            {!isLastLevel && passed && (
              <button
                onClick={onContinue}
                className="flex-1 bg-retro-accent hover:bg-[#e03a2e] text-white border-[4px] border-retro-ink rounded-2xl py-4 sm:py-6 px-4 sm:px-8 font-black text-lg sm:text-2xl uppercase tracking-wider shadow-neubrutalism-lg transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-4 group"
              >
                <span>Start Level {level + 1}</span>
                <i className="fa-solid fa-arrow-right text-xl sm:text-2xl group-hover:translate-x-2 transition-transform"></i>
              </button>
            )}
            <button
              onClick={onQuit}
              className={`flex-1 sm:flex-none bg-white hover:bg-retro-gray text-retro-ink border-[4px] border-retro-ink rounded-2xl py-4 sm:py-6 px-4 sm:px-8 font-black text-lg sm:text-2xl uppercase tracking-wider shadow-neubrutalism-lg transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-3 ${!isLastLevel && passed ? "" : "flex-1"}`}
            >
              <i className="fa-solid fa-flag-checkered text-lg sm:text-xl"></i>
              <span>{isLastLevel ? "View Results" : "Quit"}</span>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
