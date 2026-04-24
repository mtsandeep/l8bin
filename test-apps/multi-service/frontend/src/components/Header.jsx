export default function Header({ onLeaderboard, onHome }) {
  return (
    <nav className="w-full border-b-[3px] border-retro-ink bg-retro-bg sticky top-0 z-50">
      <div className="max-w-[1440px] mx-auto px-6 h-20 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 bg-retro-accent border-[3px] border-retro-ink rounded-lg shadow-neubrutalism-sm flex items-center justify-center">
            <i className="fa-solid fa-cookie-bite text-white text-xl"></i>
          </div>
          <span className="font-mono font-bold text-2xl tracking-tight">SNACK IQ</span>
        </div>
        {onLeaderboard && (
          <button
            onClick={onLeaderboard}
            className="flex items-center gap-2 bg-white hover:bg-retro-gray text-retro-ink border-[3px] border-retro-ink rounded-xl px-4 py-2 font-mono font-bold text-sm uppercase tracking-wider shadow-neubrutalism-sm transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none"
          >
            <i className="fa-solid fa-ranking-star text-retro-blue"></i>
            <span className="hidden sm:inline">Leaderboard</span>
          </button>
        )}
        {onHome && (
          <button
            onClick={onHome}
            className="flex items-center gap-2 bg-white hover:bg-retro-gray text-retro-ink border-[3px] border-retro-ink rounded-xl px-4 py-2 font-mono font-bold text-sm uppercase tracking-wider shadow-neubrutalism-sm transition-all hover:shadow-neubrutalism active:translate-y-1 active:shadow-none"
          >
            <i className="fa-solid fa-arrow-left"></i>
            <span className="hidden sm:inline">Back</span>
          </button>
        )}
      </div>
    </nav>
  );
}
