import { useState, useRef, useEffect } from "react";

export default function FinalResultsModal({ result, onClose }) {
  const {
    totalScore,
    title,
    rank,
    levelsCompleted,
    levelScores,
    correctCount,
    avgAccuracy,
    avgTime,
  } = result;
  const maxScore = 10800;
  const pct = Math.round((totalScore / maxScore) * 100);
  const [breakdownOpen, setBreakdownOpen] = useState(false);

  // Use correctCount from API, fallback to calculation if not available
  const totalCorrect =
    correctCount ?? levelScores?.filter((ls) => ls.score > 0).length ?? 0;
  const accuracy = avgAccuracy ?? pct;
  const displayAvgTime = avgTime ?? 0;

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center p-4">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-retro-ink/50 backdrop-blur-md"></div>

      {/* Modal Content */}
      <div className="bg-white border-[8px] border-retro-ink rounded-3xl p-6 lg:p-8 max-w-3xl w-full mx-auto shadow-neubrutalism-lg relative z-10 flex flex-col max-h-[90vh] overflow-y-auto">
        {/* Close Button */}
        <button
          onClick={onClose}
          className="lg:absolute lg:top-6 lg:right-6 w-10 h-10 shrink-0 border-[3px] border-retro-ink rounded-full flex items-center justify-center hover:bg-retro-ink hover:text-white transition-colors z-20 self-end lg:self-auto mb-2"
        >
          <i className="fa-solid fa-xmark text-xl"></i>
        </button>

        {/* Header */}
        <div className="text-center mb-6">
          <div className="inline-block px-6 py-2 border-[3px] border-retro-ink bg-[#FFF9C4] rounded-full font-mono font-bold text-lg shadow-neubrutalism-sm mb-6 transform -rotate-2">
            <i className="fa-solid fa-star text-retro-accent"></i> FINAL SCORE{" "}
            <i className="fa-solid fa-star text-retro-accent"></i>
          </div>
          <h2 className="text-6xl lg:text-8xl font-black font-sans text-retro-ink tracking-tighter leading-none mb-4">
            {totalScore}{" "}
            <span className="text-3xl lg:text-4xl text-retro-muted font-bold">
              PTS
            </span>
          </h2>
          <p className="font-mono text-xl text-retro-text font-bold">
            You ranked{" "}
            <span className="text-retro-accent text-2xl underline decoration-[3px] underline-offset-4">
              #{rank}
            </span>{" "}
            on the leaderboard!
          </p>
          <p className="font-mono text-lg text-retro-muted mt-2">{title}</p>
        </div>

        {/* Stats Grid */}
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 sm:gap-4 mb-6">
          <div className="border-[3px] border-retro-ink rounded-xl p-3 bg-green-50 shadow-neubrutalism-sm text-center">
            <div className="w-8 h-8 bg-green-500 border-[2px] border-retro-ink rounded-full mx-auto flex items-center justify-center text-white text-sm mb-2">
              <i className="fa-solid fa-layer-group"></i>
            </div>
            <div className="text-2xl font-black font-mono">
              {levelsCompleted}
            </div>
            <div className="text-xs font-bold uppercase text-retro-text">
              Levels
            </div>
          </div>

          <div className="border-[3px] border-retro-ink rounded-xl p-3 bg-blue-50 shadow-neubrutalism-sm text-center">
            <div className="w-8 h-8 bg-retro-blue border-[2px] border-retro-ink rounded-full mx-auto flex items-center justify-center text-white text-sm mb-2">
              <i className="fa-solid fa-check"></i>
            </div>
            <div className="text-2xl font-black font-mono">
              {totalCorrect}/9
            </div>
            <div className="text-xs font-bold uppercase text-retro-text">
              Correct
            </div>
          </div>

          <div className="border-[3px] border-retro-ink rounded-xl p-3 bg-purple-50 shadow-neubrutalism-sm text-center">
            <div className="w-8 h-8 bg-purple-500 border-[2px] border-retro-ink rounded-full mx-auto flex items-center justify-center text-white text-sm mb-2">
              <i className="fa-solid fa-bullseye"></i>
            </div>
            <div className="text-2xl font-black font-mono">{accuracy}%</div>
            <div className="text-xs font-bold uppercase text-retro-text truncate">
              Accuracy
            </div>
          </div>

          <div className="border-[3px] border-retro-ink rounded-xl p-3 bg-orange-50 shadow-neubrutalism-sm text-center">
            <div className="w-8 h-8 bg-orange-500 border-[2px] border-retro-ink rounded-full mx-auto flex items-center justify-center text-white text-sm mb-2">
              <i className="fa-solid fa-stopwatch"></i>
            </div>
            <div className="text-2xl font-black font-mono">
              {displayAvgTime.toFixed(1)}s
            </div>
            <div className="text-xs font-bold uppercase text-retro-text truncate">
              Avg Time
            </div>
          </div>
        </div>

        {/* Level Breakdown */}
        {levelScores && levelScores.length > 0 && (
          <div className="border-[3px] border-retro-ink rounded-2xl mb-6 shadow-neubrutalism-sm overflow-clip">
            <button
              onClick={() => setBreakdownOpen(!breakdownOpen)}
              className="w-full bg-retro-blue text-white px-4 py-2 font-mono font-bold text-sm flex items-center justify-between hover:bg-blue-700 transition-colors border-b-[3px] border-retro-ink"
            >
              <span>
                <i className="fa-solid fa-chart-bar mr-2"></i> LEVEL BREAKDOWN
              </span>
              <i
                className={`fa-solid fa-chevron-down transition-transform duration-200 ${breakdownOpen ? "rotate-180" : ""}`}
              ></i>
            </button>
            <div
              className="grid transition-all duration-300"
              style={{ gridTemplateRows: breakdownOpen ? "1fr" : "0fr" }}
            >
              <div className="min-h-0 overflow-hidden">
                <div className="p-3 space-y-2">
                  {levelScores.map((ls) => (
                    <div
                      key={ls.level}
                      className="flex items-center justify-between p-2 border-[2px] border-retro-ink rounded-lg"
                    >
                      <div className="flex items-center gap-3">
                        <div className="w-8 h-8 bg-retro-gray border-[2px] border-retro-ink rounded-full flex items-center justify-center font-mono font-bold text-sm">
                          {ls.level}
                        </div>
                        <span className="font-bold">Level {ls.level}</span>
                      </div>
                      <div className="flex items-center gap-3">
                        <span
                          className={`font-mono font-bold ${ls.passed ? "text-green-600" : "text-red-600"}`}
                        >
                          {ls.passed ? "Passed" : "Failed"}
                        </span>
                        <span className="font-black font-mono">{ls.score}</span>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </div>
        )}

        {/* CTAs */}
        <div className="flex justify-center mt-auto w-full">
          <button
            onClick={onClose}
            className="bg-retro-accent text-white border-[4px] border-retro-ink rounded-xl py-5 px-8 font-mono font-black text-xl uppercase tracking-wider shadow-neubrutalism hover:shadow-neubrutalism-lg transition-all active:translate-y-1 active:shadow-none flex items-center justify-center gap-3 w-full sm:w-auto"
          >
            <i className="fa-solid fa-rotate-right"></i> Play Again
          </button>
        </div>
      </div>
    </div>
  );
}
