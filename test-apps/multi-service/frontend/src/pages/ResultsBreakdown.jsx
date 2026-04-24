export default function ResultsBreakdown({
  breakdown,
  score,
  passed,
  level,
  onContinue,
  onQuit,
}) {
  const handleContinue = () => {
    if (passed && level < 3) {
      onContinue();
    } else {
      onQuit();
    }
  };

  return (
    <div className="p-4 sm:p-4 lg:p-8 flex flex-col min-h-[calc(100vh-80px)]">
      <div className="max-w-4xl mx-auto w-full flex flex-col flex-grow">
        {/* Header */}
        <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-3 mb-6 sm:mb-8 animate-fade-in-up">
          <div className="inline-flex items-center gap-2 border-[3px] border-retro-ink bg-retro-blue text-white font-mono font-bold px-3 py-1 sm:px-4 sm:py-1.5 rounded-full text-xs sm:text-sm shadow-neubrutalism-sm">
            <i className="fa-solid fa-chart-simple text-xs sm:text-sm"></i>
            <span className="text-xs sm:text-sm">RESULTS BREAKDOWN</span>
          </div>
          <div className="flex items-center gap-2 w-full sm:w-auto">
            <div className="inline-flex items-center gap-2 font-mono font-bold border-[3px] border-retro-ink bg-white text-retro-ink px-4 py-1.5 rounded-full shadow-neubrutalism-sm">
              <i className="fa-solid fa-fire text-orange-500"></i>
              <span>{score} pts</span>
            </div>
            <button
              onClick={handleContinue}
              className="bg-retro-accent hover:bg-[#e03a2e] text-white border-[2px] border-retro-ink rounded-lg py-1.5 px-3 font-mono font-bold text-xs sm:text-sm uppercase tracking-wider shadow-neubrutalism-sm transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none flex items-center gap-2"
            >
              <span>{passed && level < 3 ? "Continue" : "View Results"}</span>
              <i
                className={`fa-solid ${passed && level < 3 ? "fa-arrow-right" : "fa-flag-checkered"}`}
              ></i>
            </button>
          </div>
        </div>

        {/* Results Grid */}
        <div className="grid grid-cols-1 gap-4 sm:gap-6 mb-6 sm:mb-8">
          {breakdown.map((result, index) => {
            const diffColor =
              result.differencePct <= 5
                ? "text-green-600"
                : result.differencePct <= 10
                  ? "text-blue-600"
                  : result.differencePct <= 20
                    ? "text-yellow-600"
                    : "text-red-600";
            const diffIcon =
              result.differencePct <= 5
                ? "fa-bullseye"
                : result.differencePct <= 10
                  ? "fa-check"
                  : result.differencePct <= 20
                    ? "fa-minus"
                    : "fa-xmark";

            // Calculate time bonus
            let timeBonus = 0;
            if (result.timeTaken > 0 && result.timeTaken < 5) timeBonus = 500;
            else if (result.timeTaken > 0 && result.timeTaken < 10)
              timeBonus = 200;

            // Calculate base points (total - time bonus)
            const basePoints = result.points - timeBonus;

            return (
              <div
                key={result.foodId}
                className="bg-white border-[3px] border-retro-ink rounded-2xl p-4 sm:p-6 shadow-neubrutalism-lg animate-fade-in-up"
                style={{ animationDelay: `${index * 0.3}s` }}
              >
                {/* Question Header */}
                <div className="flex items-center justify-between mb-3 sm:mb-4">
                  <h3 className="text-lg sm:text-xl font-black font-sans">
                    {result.foodName}
                  </h3>
                  <div
                    className={`inline-flex items-center gap-2 border-[2px] border-retro-ink rounded-full px-3 py-1 shadow-neubrutalism-sm ${
                      result.points >= 800
                        ? "bg-green-500 text-white"
                        : result.points >= 400
                          ? "bg-retro-blue text-white"
                          : result.points > 0
                            ? "bg-yellow-500 text-retro-ink"
                            : "bg-retro-gray text-retro-ink"
                    }`}
                  >
                    <i
                      className={`fa-solid ${diffIcon} text-sm ${
                        result.points >= 400 ? "text-white" : "text-retro-ink"
                      }`}
                    ></i>
                    <span
                      className={`font-mono font-black text-sm ${
                        result.points >= 400 ? "text-white" : "text-retro-ink"
                      }`}
                    >
                      +{result.points}
                    </span>
                  </div>
                </div>

                {/* Stats */}
                <div className="grid grid-cols-3 gap-2 sm:gap-4 mb-3">
                  <div className="bg-retro-gray border-[2px] border-retro-ink rounded-lg p-2 sm:p-3 text-center">
                    <div className="font-mono text-xs text-retro-muted uppercase mb-1">
                      Correct
                    </div>
                    <div className="font-black text-lg sm:text-xl text-retro-ink">
                      {result.correctCalories}
                    </div>
                  </div>
                  <div className="bg-retro-gray border-[2px] border-retro-ink rounded-lg p-2 sm:p-3 text-center">
                    <div className="font-mono text-xs text-retro-muted uppercase mb-1">
                      Your Guess
                    </div>
                    <div className="font-black text-lg sm:text-xl text-retro-ink">
                      {result.userGuess}
                    </div>
                  </div>
                  <div className="bg-retro-gray border-[2px] border-retro-ink rounded-lg p-2 sm:p-3 text-center">
                    <div className="font-mono text-xs text-retro-muted uppercase mb-1">
                      Diff
                    </div>
                    <div
                      className={`font-black text-lg sm:text-xl ${diffColor}`}
                    >
                      {result.difference > 0 ? "+" : ""}
                      {result.difference}
                    </div>
                    <div className={`font-mono text-xs ${diffColor}`}>
                      {result.differencePct}%
                      {result.differencePct > 50 && (
                        <span className="text-retro-muted ml-1">
                          capped at 50%
                        </span>
                      )}
                    </div>
                  </div>
                </div>

                {/* Time & Points Breakdown */}
                <div className="flex items-center justify-between gap-2 sm:gap-4">
                  <div className="flex items-center gap-2">
                    <div className="bg-retro-blue border-[2px] border-retro-ink rounded-lg px-3 py-1.5 flex items-center gap-2">
                      <i className="fa-solid fa-stopwatch text-white text-sm"></i>
                      <span className="font-mono font-bold text-white text-sm">
                        {result.timeTaken.toFixed(1)}s
                      </span>
                    </div>
                    {timeBonus > 0 && (
                      <div className="bg-yellow-500 border-[2px] border-retro-ink rounded-lg px-3 py-1.5 flex items-center gap-2">
                        <i className="fa-solid fa-bolt text-retro-ink text-sm"></i>
                        <span className="font-mono font-bold text-retro-ink text-sm">
                          +{timeBonus}
                        </span>
                      </div>
                    )}
                  </div>
                  <div className="font-mono text-xs text-retro-text">
                    <span className="text-retro-muted">Base:</span> {basePoints}
                    {timeBonus > 0 && (
                      <span>
                        {" "}
                        +{" "}
                        <span className="text-yellow-600">
                          Time: {timeBonus}
                        </span>
                      </span>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>

        {/* Total Summary */}
        <div
          className="bg-retro-blue border-[3px] border-retro-ink rounded-2xl p-4 sm:p-6 mb-6 sm:mb-8 animate-fade-in-up"
          style={{ animationDelay: "1s" }}
        >
          <div className="flex items-center justify-between text-white mb-3">
            <div>
              <div className="font-mono text-sm uppercase opacity-90 mb-1">
                Total Score
              </div>
              <div className="font-black text-3xl sm:text-4xl">{score} pts</div>
            </div>
            <div className="text-right">
              <div className="font-mono text-sm uppercase opacity-90 mb-1">
                Result
              </div>
              <div className="font-black text-3xl sm:text-4xl">
                {passed ? "PASSED" : "FAILED"}
              </div>
            </div>
          </div>
          <div className="text-white text-center font-mono text-sm opacity-90">
            {passed ? (
              <span>Avg diff &le; 30% - Great job!</span>
            ) : (
              <span>
                Avg diff &gt; 30% - Need avg less than 30% to continue (max
                penalty per question: 50%)
              </span>
            )}
          </div>
        </div>

        {/* Continue/Quit Buttons */}
        <div
          className="mt-auto pt-4 sm:pt-8 border-t-[3px] border-retro-ink border-dashed flex justify-center gap-3 sm:gap-4 animate-fade-in-up"
          style={{ animationDelay: "1.2s" }}
        >
          {passed && (
            <button
              onClick={onQuit}
              className="w-full sm:w-auto bg-retro-gray hover:bg-gray-400 text-retro-ink border-[3px] border-retro-ink rounded-xl py-3 sm:py-4 px-4 sm:px-8 font-mono font-black text-base sm:text-lg uppercase tracking-wider shadow-neubrutalism transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-3"
            >
              <span>Quit</span>
              <i className="fa-solid fa-flag-checkered"></i>
            </button>
          )}
          <button
            onClick={handleContinue}
            className="w-full sm:w-auto bg-retro-accent hover:bg-[#e03a2e] text-white border-[3px] border-retro-ink rounded-xl py-3 sm:py-4 px-4 sm:px-8 font-mono font-black text-base sm:text-lg uppercase tracking-wider shadow-neubrutalism transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-3"
          >
            <span>{passed && level < 3 ? "Continue" : "View Results"}</span>
            <i
              className={`fa-solid ${passed && level < 3 ? "fa-arrow-right" : "fa-flag-checkered"}`}
            ></i>
          </button>
        </div>
      </div>
    </div>
  );
}
