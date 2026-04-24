import { useState, useEffect } from "react";
import {
  fetchLeaderboard,
  subscribeLeaderboard,
  fetchStats,
} from "../services/api";

export default function Leaderboard({ refreshKey }) {
  const [entries, setEntries] = useState([]);
  const [loading, setLoading] = useState(true);
  const [stats, setStats] = useState({ totalGamesPlayed: 0, totalSessions: 0 });

  useEffect(() => {
    load();
  }, [refreshKey]);

  useEffect(() => {
    const unsubscribe = subscribeLeaderboard((update) => {
      setEntries((prev) => [
        update,
        ...prev.filter(
          (e) => !(e.name === update.name && e.score === update.score),
        ),
      ]);
    });
    return unsubscribe;
  }, []);

  async function load() {
    setLoading(true);
    try {
      const [data, statsData] = await Promise.all([
        fetchLeaderboard(),
        fetchStats(),
      ]);
      setEntries(data);
      setStats(statsData);
    } catch {}
    setLoading(false);
  }

  return (
    <div className="flex flex-col min-h-[calc(100vh-80px)]">
      {/* Header */}
      <div className="p-6 border-b-[3px] border-retro-ink bg-retro-blue text-white">
        <h2 className="font-black text-2xl uppercase tracking-wide flex items-center gap-3">
          <i className="fa-solid fa-ranking-star text-retro-accent"></i> Top
          Players
        </h2>
        <p className="font-mono text-sm mt-1 opacity-90">
          All-time high scores • {stats.totalGamesPlayed} games played
        </p>
      </div>

      {/* List */}
      <div className="flex-grow overflow-y-auto p-4 space-y-3">
        {loading ? (
          <div className="text-center py-10 font-mono text-retro-muted text-sm">
            Loading...
          </div>
        ) : entries.length === 0 ? (
          <div className="text-center py-10 font-mono text-retro-muted text-sm">
            No scores yet. Be the first to play!
          </div>
        ) : (
          entries.map((entry, i) => {
            const isTop3 = i < 3;
            const bgColor =
              i === 0
                ? "bg-[#FFF9C4]"
                : i === 1
                  ? "bg-white"
                  : i === 2
                    ? "bg-white"
                    : "";

            return isTop3 ? (
              <div
                key={`${entry.name}-${entry.score}-${i}`}
                className={`flex items-center gap-4 p-3 border-[3px] border-retro-ink rounded-xl ${bgColor} shadow-neubrutalism-sm relative overflow-hidden`}
              >
                {i === 0 && (
                  <div className="absolute -right-4 -top-4 text-6xl opacity-10">
                    <i className="fa-solid fa-crown"></i>
                  </div>
                )}
                <div
                  className={`w-8 h-8 flex items-center justify-center font-black text-xl ${
                    i === 0
                      ? "text-retro-accent"
                      : i === 1
                        ? "text-[#9CA3AF]"
                        : "text-[#B45309]"
                  }`}
                >
                  #{i + 1}
                </div>
                <div className="w-10 h-10 rounded-full border-2 border-retro-ink bg-retro-gray flex items-center justify-center font-mono font-bold text-sm">
                  {entry.name.slice(0, 2).toUpperCase()}
                </div>
                <div className="flex-grow min-w-0">
                  <div className="font-bold text-sm truncate">{entry.name}</div>
                  <div className="font-mono text-xs text-retro-text truncate">
                    {entry.levels_completed
                      ? `Lvl ${entry.levels_completed}/3`
                      : ""}
                  </div>
                </div>
                <div className="font-black font-mono text-lg">
                  {entry.score.toLocaleString()}
                </div>
              </div>
            ) : (
              <div
                key={`${entry.name}-${entry.score}-${i}`}
                className="flex items-center gap-4 p-2 hover:bg-retro-gray rounded-lg transition-colors"
              >
                <div className="w-8 text-center font-mono font-bold text-sm text-retro-muted">
                  {i + 1}
                </div>
                <div className="w-8 h-8 rounded-full border border-retro-ink bg-retro-gray flex items-center justify-center font-mono text-xs font-bold">
                  {entry.name.slice(0, 2).toUpperCase()}
                </div>
                <div className="flex-grow font-bold text-sm truncate">
                  {entry.name}
                </div>
                <div className="font-mono font-bold text-sm">
                  {entry.score.toLocaleString()}
                </div>
              </div>
            );
          })
        )}
      </div>

      {/* Scoring Info Footer */}
      <div className="p-6 border-t-[3px] border-retro-ink bg-retro-bg">
        <div className="border-[3px] border-retro-ink rounded-xl p-4 bg-white shadow-neubrutalism-sm">
          <h4 className="font-black text-sm uppercase mb-2 flex items-center gap-2">
            <i className="fa-solid fa-circle-question text-retro-blue"></i> How
            Scoring Works
          </h4>
          <ul className="font-mono text-xs text-retro-text space-y-2">
            <li className="flex items-start gap-2">
              <i className="fa-solid fa-check text-green-600 mt-0.5"></i>
              <span>Exact match = 1000 pts</span>
            </li>
            <li className="flex items-start gap-2">
              <i className="fa-solid fa-bullseye text-orange-500 mt-0.5"></i>
              <span>Within 20% = 400 pts</span>
            </li>
            <li className="flex items-start gap-2">
              <i className="fa-solid fa-bolt text-yellow-500 mt-0.5"></i>
              <span>Speed bonus: Up to 200 pts</span>
            </li>
          </ul>
        </div>
      </div>
    </div>
  );
}
