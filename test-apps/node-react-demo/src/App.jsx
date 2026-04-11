import { useState, useEffect } from "react";

function StatusBadge({ status }) {
  const color = status === "ok" ? "#22c55e" : "#ef4444";
  return (
    <span
      style={{
        display: "inline-block",
        padding: "2px 10px",
        borderRadius: "9999px",
        fontSize: "0.75rem",
        fontWeight: 600,
        background: `${color}22`,
        color: color,
        border: `1px solid ${color}44`,
      }}
    >
      {status}
    </span>
  );
}

export default function App() {
  const [info, setInfo] = useState(null);
  const [loading, setLoading] = useState(true);
  const [idleSeconds, setIdleSeconds] = useState(0);
  const [now, setNow] = useState(new Date());

  // One-time fetch — no polling
  useEffect(() => {
    Promise.all([
      fetch("/api/health?visit").then((r) => r.json()),
      fetch("/api/info").then((r) => r.json()),
    ])
      .then(([h, i]) => {
        setInfo({ ...i, uptime: h.uptime, visitCount: h.visitCount });
      })
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  // Live tickers — pure client-side
  useEffect(() => {
    const interval = setInterval(() => {
      setIdleSeconds((s) => s + 1);
      setNow(new Date());
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  const IDLE_LIMIT = 60;
  const BUFFER_LIMIT = 30;
  const inBuffer = idleSeconds >= IDLE_LIMIT;
  const isSlept = idleSeconds >= IDLE_LIMIT + BUFFER_LIMIT;
  const idleRemaining = Math.max(IDLE_LIMIT - idleSeconds, 0);
  const bufferRemaining = Math.max(IDLE_LIMIT + BUFFER_LIMIT - idleSeconds, 0);
  const idlePercent = Math.min((idleSeconds / IDLE_LIMIT) * 100, 100);
  const idleColor = idleSeconds < 45 ? "#22c55e" : "#eab308";

  if (loading) {
    return <p style={{ textAlign: "center", marginTop: "4rem" }}>Loading...</p>;
  }

  return (
    <div>
      <header style={{ textAlign: "center", marginBottom: "2rem" }}>
        <h1 style={{ fontSize: "1.75rem", fontWeight: 700 }}>
          LiteBin Test App
        </h1>
        <p style={{ color: "#888", marginTop: "0.25rem" }}>
          Express + React demo app
        </p>
      </header>

      {/* Server Info */}
      {info && (
        <section style={cardStyle}>
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <h2 style={headingStyle}>Server Status</h2>
            <StatusBadge status="ok" />
          </div>
          <table style={{ width: "100%", marginTop: "0.75rem" }}>
            <tbody>
              <Row label="Uptime" value={formatUptime(info.uptime)} />
              <Row label="Node" value={info.node} />
              <Row label="Env" value={info.env} />
              <Row label="Port" value={info.port} />
              <Row label="Memory" value={`${info.memory.heapUsed}MB used`} />
              <Row label="Visits" value={info.visitCount} />
              <Row label="Time" value={now.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit", second: "2-digit", hour12: true })} />
            </tbody>
          </table>
        </section>
      )}

      {/* Idle Timer */}
      <section style={{ ...cardStyle, marginTop: "1rem", position: "relative" }}>
        {isSlept && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              background: "rgba(0, 0, 0, 0.75)",
              borderRadius: "12px",
              display: "flex",
              flexDirection: "column",
              justifyContent: "center",
              alignItems: "center",
              gap: "1rem",
              zIndex: 1,
            }}
          >
            <p style={{ color: "#ef4444", fontWeight: 600, margin: 0 }}>
              Container is sleeping
            </p>
            <button
              onClick={() => location.reload()}
              style={{
                background: "#38bdf8",
                color: "#0f172a",
                border: "none",
                borderRadius: "8px",
                padding: "0.6rem 1.5rem",
                fontWeight: 600,
                cursor: "pointer",
                fontSize: "0.9rem",
              }}
            >
              Reload to wake
            </button>
          </div>
        )}
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <h2 style={headingStyle}>Time Until Sleep</h2>
          <span
            style={{
              fontSize: "0.875rem",
              fontFamily: "monospace",
              color: inBuffer ? "#a855f7" : idleColor,
            }}
          >
            {inBuffer ? `${bufferRemaining}s` : `${idleRemaining}s`}
          </span>
        </div>
        <div
          style={{
            marginTop: "0.75rem",
            height: "6px",
            borderRadius: "3px",
            overflow: "hidden",
            display: "flex",
          }}
        >
          <div style={{ height: "100%", width: "66.6%", background: "#2a2a2a" }}>
            <div
              style={{
                height: "100%",
                width: `${idlePercent}%`,
                background: idleColor,
                transition: "width 1s linear, background 0.5s",
              }}
            />
          </div>
          <div style={{ height: "100%", width: "33.3%", background: "#2a1a3b" }}>
            <div
              style={{
                height: "100%",
                width: inBuffer ? `${Math.min(((idleSeconds - IDLE_LIMIT) / BUFFER_LIMIT) * 100, 100)}%` : "0%",
                background: "#a855f7",
                transition: "width 1s linear",
              }}
            />
          </div>
        </div>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            marginTop: "0.35rem",
          }}
        >
          <span style={{ fontSize: "0.65rem", color: "#666" }}>60s idle</span>
          <span style={{ fontSize: "0.65rem", color: inBuffer ? "#a855f7" : "#666" }}>
            {inBuffer ? "janitor sweeping" : "30s buffer"}
          </span>
        </div>
        <p
          style={{
            marginTop: "0.75rem",
            fontSize: "0.7rem",
            color: "#888",
            lineHeight: 1.5,
            margin: "0.75rem 0 0",
            fontStyle: "italic",
          }}
        >
          This timer is based on your session. If another visitor wakes the
          container while you wait, it may need another 60s of inactivity
          before sleeping.
        </p>
      </section>
    </div>
  );
}

function formatUptime(seconds) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

function Row({ label, value }) {
  return (
    <tr>
      <td style={{ padding: "0.25rem 0", color: "#888", width: "40%" }}>
        {label}
      </td>
      <td style={{ padding: "0.25rem 0", fontFamily: "monospace" }}>
        {value}
      </td>
    </tr>
  );
}

const cardStyle = {
  background: "#1a1a1a",
  border: "1px solid #2a2a2a",
  borderRadius: "12px",
  padding: "1.25rem",
};

const headingStyle = {
  fontSize: "1rem",
  fontWeight: 600,
  color: "#ccc",
};
