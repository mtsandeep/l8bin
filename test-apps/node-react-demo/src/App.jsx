import { useEffect, useRef, useState } from "react";

// The demo's timeline, mirroring what litebin does to this container:
// 60s with no traffic -> eligible to sleep; the janitor sweeps every 30s,
// so the container stops somewhere between 60s and 90s after the last request.
const IDLE_LIMIT = 60;
const SWEEP_EVERY = 30;
const TOTAL = IDLE_LIMIT + SWEEP_EVERY;

export default function App() {
  const [info, setInfo] = useState(null);
  const [loading, setLoading] = useState(true);
  const [idleSeconds, setIdleSeconds] = useState(0);
  const [asleepFor, setAsleepFor] = useState(0);
  const [pinged, setPinged] = useState(false);
  const [waking, setWaking] = useState(false);

  useEffect(() => {
    Promise.all([
      pollHealth(),
      fetch("/api/info").then((r) => r.json()),
    ])
      .then(([h, i]) => setInfo({ ...i, uptime: h.uptime, visits: h.visitCount }))
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    const t = setInterval(() => {
      setIdleSeconds((s) => s + 1);
      if (idleSecondsRef.current >= TOTAL) setAsleepFor((a) => a + 1);
    }, 1000);
    return () => clearInterval(t);
  }, []);

  const idleSecondsRef = useRef(0);
  idleSecondsRef.current = Math.min(idleSeconds, TOTAL);
  const awaitingSweep = idleSeconds >= IDLE_LIMIT && idleSeconds < TOTAL;
  const isAsleep = idleSeconds >= TOTAL;

  // "Keep it awake" — real traffic to the server, which is exactly what
  // resets litebin's timer in production. A 503 means the container is still
  // starting (cold wake): keep polling in the background until it answers.
  const keepAwake = async () => {
    setWaking(true);
    setIdleSeconds(0);
    setAsleepFor(0);
    try {
      const h = await pollHealth();
      if (h) {
        setInfo((prev) => (prev ? { ...prev, visits: h.visitCount, uptime: h.uptime } : prev));
        setPinged(true);
        setTimeout(() => setPinged(false), 1500);
      }
    } finally {
      setWaking(false);
    }
  };

  if (loading) return <p className="loading">waking…</p>;

  return (
    <div className="page">
      <header className="masthead">
        <div className="brand-row">
          <a className="brand" href="https://l8bin.com">
            <img className="brand-mark" src="/logo.svg" alt="" />
            <span className="brand-name">L8BIN<span className="brand-dot">.</span></span>
          </a>
          <span className="demo-pill">
            <span className="demo-pill-tag">React + Express</span>
            Aggressive Sleep Demo
          </span>
        </div>
        <p className="tagline">
          A tiny app living inside a <em>scale-to-zero</em> container.
          <br className="hide-sm" /> No traffic for a minute or so and it stops existing. Reload to feel it wake.
        </p>
      </header>

      <section className="hero" data-phase={isAsleep ? "asleep" : awaitingSweep ? "sweep" : "active"}>
        {isAsleep && (
          <SleepOverlay asleepFor={asleepFor} />
        )}
        <SleepRing idleSeconds={idleSeconds} awaitingSweep={awaitingSweep} />
        <div className="hero-side">
          <p className="hero-copy">
            {awaitingSweep ? (
              <>Idle past the limit — <strong>this container is eligible to sleep</strong>. The janitor sweeps every 30 seconds and stops it on the next pass. Any request now cancels that.</>
            ) : (
              <>Every request resets this clock. At 60s idle the container becomes eligible to sleep, and litebin's next sweep <strong>stops it and frees its RAM</strong>.</>
            )}
          </p>
          <button type="button" className="keep-awake" onClick={keepAwake} disabled={waking}>
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>
            {waking ? "starting — waiting for the container…" : pinged ? "traffic received — clock reset" : "send a request (resets the clock)"}
          </button>
          <p className="hero-note">
            This is the exact mechanism production litebin uses — the button is a real request to this server.
          </p>
        </div>
      </section>

      <PhaseStrip awaitingSweep={awaitingSweep} isAsleep={isAsleep} />

      {info && (
        <section className="stats">
          <Stat label="awake for" value={formatUptime(info.uptime + Math.min(idleSeconds, TOTAL))} accent={!isAsleep} stopped={isAsleep} />
          <Stat label="visits" value={info.visits} mono />
          <Stat label="node" value={info.node.replace("v", "")} mono />
          <Stat label="heap" value={`${info.memory.heapUsed} MB`} mono />
        </section>
      )}

      <footer className="foot">
        Runs on <a className="wordmark wordmark-sm" href="https://l8bin.com">l8<span>bin</span></a> — deploy your own with one command.
      </footer>
    </div>
  );
}

// --- API --------------------------------------------------------------------

// Fetch /api/health, retrying in the background while it returns 503
// (container still starting after a cold wake). Gives up after ~60s.
async function pollHealth() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch("/api/health");
      if (r.ok) return await r.json();
    } catch {
      /* network hiccup during wake — keep polling */
    }
    await new Promise((res) => setTimeout(res, 1000));
  }
  throw new Error("health check never became ready");
}

// --- The countdown ring ---------------------------------------------------

function SleepRing({ idleSeconds, awaitingSweep }) {
  const R = 84;
  const C = 2 * Math.PI * R;
  const remaining = Math.max(TOTAL - idleSeconds, 0);
  const shown = awaitingSweep ? Math.max(TOTAL - idleSeconds, 0) : Math.max(IDLE_LIMIT - idleSeconds, 0);

  return (
    <div className="ring-wrap" role="timer" aria-label={`${shown} seconds until sleep`}>
      <svg viewBox="0 0 200 200" className="ring">
        <circle className="ring-track" cx="100" cy="100" r={R} />
        <circle
          className="ring-progress"
          cx="100" cy="100" r={R}
          strokeDasharray={C}
          strokeDashoffset={C * (1 - remaining / TOTAL)}
        />
        {/* marker at 60s of the 90s arc — where sleep becomes eligible */}
        <line
          className="ring-marker"
          x1="100" y1="10" x2="100" y2="24"
          transform="rotate(-150 100 100)"
        />
      </svg>
      <div className="ring-center">
        <span className="ring-count" data-sweep={awaitingSweep}>{shown}</span>
        <span className="ring-unit">sec</span>
        <span className="ring-label">{awaitingSweep ? "to next sweep" : "to sleep eligibility"}</span>
      </div>
      <div className="ring-caption">
        <span className="seg seg-idle">60s idle</span>
        <span className="seg seg-sweep">+0–30s for sweep</span>
      </div>
    </div>
  );
}

// --- The three-phase story ------------------------------------------------

function PhaseStrip({ awaitingSweep, isAsleep }) {
  const step = isAsleep ? 3 : awaitingSweep ? 2 : 1;
  return (
    <section className="phases">
      <div className="phase" data-on={step === 1}>
        <span className="phase-dot" />
        <div>
          <strong>Traffic keeps it alive</strong>
          <p>Any request — yours or a stranger's — resets the timer.</p>
        </div>
      </div>
      <div className="phase" data-on={step === 2}>
        <span className="phase-dot" />
        <div>
          <strong>60s idle → sleep eligible</strong>
          <p>The janitor sweeps every 30 seconds and stops eligible containers — so sleep lands somewhere in the next sweep.</p>
        </div>
      </div>
      <div className="phase" data-on={step === 3}>
        <span className="phase-dot" />
        <div>
          <strong>Stopped. RAM freed.</strong>
          <p>The sweep stops the container and frees its RAM. The next request wakes it in a few seconds.</p>
        </div>
      </div>
    </section>
  );
}

// --- The "it actually slept" moment ----------------------------------------

function SleepOverlay({ asleepFor }) {
  return (
    <div className="sleep-overlay">
      <div className="sleep-box">
        <span className="sleep-cursor" aria-hidden="true" />
        <p className="sleep-title">container stopped</p>
        <p className="sleep-sub">
          as it would be in production — asleep for <strong>{formatUptime(asleepFor)}</strong>
        </p>
        <button type="button" onClick={() => location.reload()}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/></svg>
          reload page or click here to wake
        </button>
        <p className="sleep-note">
          In production, litebin's waker does this for you: the next URL hit
          starts the container and serves the request within seconds.
        </p>
      </div>
    </div>
  );
}

// --- Bits -------------------------------------------------------------------

function Stat({ label, value, mono, accent, stopped }) {
  return (
    <div className={`stat${accent ? " stat-accent" : ""}${mono ? " stat-mono" : ""}${stopped ? " stat-stopped" : ""}`}>
      <span className="stat-label">{label}</span>
      <span className="stat-value">
        {value}
        {stopped && <span className="stat-stopped-tag">[stopped]</span>}
      </span>
    </div>
  );
}

function formatUptime(seconds) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}
