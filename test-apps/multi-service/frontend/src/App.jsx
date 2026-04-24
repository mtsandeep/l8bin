import { useState, useCallback } from "react";
import Header from "./components/Header";
import Footer from "./components/Footer";
import Leaderboard from "./pages/Leaderboard";
import StartScreen from "./pages/StartScreen";
import LevelIntro from "./pages/LevelIntro";
import QuestionScreen from "./pages/QuestionScreen";
import ResultsBreakdown from "./pages/ResultsBreakdown";
import LevelComplete from "./pages/LevelComplete";
import FinalResultsModal from "./pages/FinalResultsModal";
import { fetchQuiz, submitLevelResult, finishGame } from "./services/api";

export default function App() {
  const [screen, setScreen] = useState("start");
  const [sessionId, setSessionId] = useState(null);
  const [playerName, setPlayerName] = useState("");
  const [currentLevel, setCurrentLevel] = useState(1);
  const [questions, setQuestions] = useState([]);
  const [levelResult, setLevelResult] = useState(null);
  const [finalResult, setFinalResult] = useState(null);
  const [leaderboardKey, setLeaderboardKey] = useState(0);
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(false);
  const [breakdown, setBreakdown] = useState(null);

  const resetGame = useCallback(() => {
    setSessionId(null);
    setCurrentLevel(1);
    setQuestions([]);
    setLevelResult(null);
    setFinalResult(null);
    setBreakdown(null);
    setError(null);
    setLoading(false);
    setScreen("start");
  }, []);

  const handleStart = useCallback(async (name) => {
    setPlayerName(name);
    setCurrentLevel(1);
    setScreen("levelIntro");
  }, []);

  const handleStartLevel = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await fetchQuiz(currentLevel, sessionId, playerName);
      setSessionId(data.sessionId);
      setQuestions(data.questions);
      setScreen("question");
    } catch (err) {
      setError("Failed to load questions. Please try again.");
    } finally {
      setLoading(false);
    }
  }, [currentLevel, sessionId, playerName]);

  const handleLevelComplete = useCallback(
    async (answers) => {
      setLoading(true);
      setError(null);
      try {
        const result = await submitLevelResult(
          sessionId,
          answers,
          currentLevel,
        );
        // Always show breakdown first, regardless of pass/fail
        setLevelResult(result);
        setBreakdown(result.breakdown);
        setScreen("resultsBreakdown");
      } catch (err) {
        setError("Failed to submit answers. Please try again.");
        setLoading(false);
      } finally {
        setLoading(false);
      }
    },
    [sessionId, currentLevel, playerName],
  );

  const handleContinue = useCallback(() => {
    setCurrentLevel((prev) => prev + 1);
    setLevelResult(null);
    setBreakdown(null);
    setScreen("levelIntro");
  }, []);

  const handleQuit = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await finishGame(sessionId, playerName);
      setFinalResult(result);
      setScreen("finalResults");
      setLeaderboardKey((k) => k + 1);
    } catch (err) {
      setError("Failed to save score. Please try again.");
    } finally {
      setLoading(false);
    }
  }, [sessionId, playerName]);

  const showSidebar = screen === "start";

  return (
    <div className="min-h-screen bg-retro-bg font-sans text-retro-ink antialiased flex flex-col">
      <Header
        onLeaderboard={
          screen === "start" ? () => setScreen("leaderboard") : null
        }
        onHome={screen === "leaderboard" ? resetGame : null}
      />

      {/* Full-page Leaderboard */}
      {screen === "leaderboard" && (
        <div className="flex-grow max-w-2xl mx-auto w-full">
          <Leaderboard refreshKey={leaderboardKey} />
          <div className="p-6 text-center">
            <button
              onClick={resetGame}
              className="bg-white hover:bg-retro-gray text-retro-ink border-[3px] border-retro-ink rounded-xl px-6 py-3 font-mono font-bold text-sm uppercase tracking-wider shadow-neubrutalism-sm transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none"
            >
              <i className="fa-solid fa-arrow-left mr-2"></i> Back to Home
            </button>
          </div>
        </div>
      )}

      {screen !== "leaderboard" && (
        <main className="flex-grow flex flex-col lg:flex-row max-w-[1440px] mx-auto w-full">
          {/* Main Game Area */}
          <section
            className={`${showSidebar ? "w-full lg:w-3/4 border-r-[3px] border-retro-ink" : "w-full"} min-h-[calc(100vh-80px)]`}
          >
            {loading && (
              <div className="flex items-center justify-center min-h-[calc(100vh-80px)]">
                <div className="text-center animate-fade-in-up">
                  <div className="text-6xl mb-4">🍕</div>
                  <p className="font-mono font-bold text-xl text-retro-text">
                    Loading...
                  </p>
                </div>
              </div>
            )}

            {error && !loading && (
              <div className="flex items-center justify-center min-h-[calc(100vh-80px)]">
                <div className="text-center">
                  <p className="font-mono text-retro-accent font-bold mb-4">
                    {error}
                  </p>
                  <button
                    onClick={() => setScreen("start")}
                    className="font-mono font-bold border-[3px] border-retro-ink px-6 py-2 rounded-lg hover:bg-retro-ink hover:text-white transition-colors"
                  >
                    Go Back
                  </button>
                </div>
              </div>
            )}

            {!loading && !error && screen === "start" && (
              <StartScreen onStart={handleStart} />
            )}

            {!loading && !error && screen === "levelIntro" && (
              <LevelIntro
                level={currentLevel}
                onStartLevel={handleStartLevel}
              />
            )}

            {!loading && !error && screen === "question" && (
              <QuestionScreen
                questions={questions}
                level={currentLevel}
                onComplete={handleLevelComplete}
              />
            )}

            {!loading &&
              !error &&
              screen === "resultsBreakdown" &&
              breakdown &&
              levelResult && (
                <ResultsBreakdown
                  breakdown={breakdown}
                  score={levelResult.score}
                  passed={levelResult.passed}
                  level={currentLevel}
                  onContinue={handleContinue}
                  onQuit={handleQuit}
                />
              )}

            {!loading &&
              !error &&
              screen === "levelComplete" &&
              levelResult && (
                <LevelComplete
                  result={levelResult}
                  level={currentLevel}
                  onContinue={handleContinue}
                  onQuit={handleQuit}
                />
              )}
          </section>

          {/* Leaderboard Sidebar — only on home screen */}
          {showSidebar && (
            <aside className="w-full lg:w-1/4 bg-white flex flex-col min-h-[calc(100vh-80px)]">
              <Leaderboard refreshKey={leaderboardKey} />
            </aside>
          )}
        </main>
      )}

      {/* Final Results Modal */}
      {screen === "finalResults" && finalResult && (
        <FinalResultsModal result={finalResult} onClose={resetGame} />
      )}

      {/* Footer */}
      <Footer />
    </div>
  );
}
