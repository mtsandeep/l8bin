import { useState, useEffect } from "react";
import { fetchStats } from "../services/api";

export default function Footer() {
  const [stats, setStats] = useState(null);

  useEffect(() => {
    const loadStats = async () => {
      try {
        const data = await fetchStats();
        setStats(data);
      } catch (err) {
        console.error("Failed to fetch stats:", err);
      }
    };
    loadStats();
  }, []);

  if (!stats) return null;

  return (
    <footer className="bg-retro-ink text-white py-3 px-4 border-t-[3px] border-retro-accent">
      <div className="max-w-[1440px] mx-auto flex flex-wrap justify-center gap-4 sm:gap-8 text-xs sm:text-sm font-mono">
        <div className="flex items-center gap-2">
          <i className="fa-solid fa-gamepad text-retro-accent"></i>
          <span className="text-retro-gray">Games:</span>
          <span className="font-bold">{stats.totalGamesPlayed}</span>
        </div>
        <div className="flex items-center gap-2">
          <i className="fa-solid fa-utensils text-retro-accent"></i>
          <span className="text-retro-gray">Foods:</span>
          <span className="font-bold">{stats.totalFoods}</span>
        </div>
        <div className="flex items-center gap-2">
          <i className="fa-solid fa-globe text-retro-accent"></i>
          <span className="text-retro-gray">Cuisines:</span>
          <span className="font-bold">{stats.totalCuisines}</span>
        </div>
      </div>
    </footer>
  );
}
