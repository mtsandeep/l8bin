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
  const [health, setHealth] = useState(null);
  const [info, setInfo] = useState(null);
  const [items, setItems] = useState([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    Promise.all([
      fetch("/api/health").then((r) => r.json()),
      fetch("/api/info").then((r) => r.json()),
      fetch("/api/items").then((r) => r.json()),
    ])
      .then(([h, i, it]) => {
        setHealth(h);
        setInfo(i);
        setItems(it);
      })
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return <p style={{ textAlign: "center", marginTop: "4rem" }}>Loading…</p>;
  }

  return (
    <div>
      <header style={{ textAlign: "center", marginBottom: "2rem" }}>
        <h1 style={{ fontSize: "1.75rem", fontWeight: 700 }}>
          🚀 LiteBin Test App
        </h1>
        <p style={{ color: "#888", marginTop: "0.25rem" }}>
          A demo project for testing the LiteBin PaaS
        </p>
      </header>

      {/* Health & Info */}
      <section style={cardStyle}>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <h2 style={headingStyle}>Server Status</h2>
          {health && <StatusBadge status={health.status} />}
        </div>
        {health && info && (
          <table style={{ width: "100%", marginTop: "0.75rem" }}>
            <tbody>
              <Row label="Uptime" value={`${health.uptime}s`} />
              <Row label="Node" value={info.node} />
              <Row label="Env" value={info.env} />
              <Row label="Port" value={info.port} />
              <Row label="Timestamp" value={health.timestamp} />
            </tbody>
          </table>
        )}
      </section>

      {/* Checklist */}
      <section style={{ ...cardStyle, marginTop: "1rem" }}>
        <h2 style={headingStyle}>Deployment Checklist</h2>
        <ul style={{ listStyle: "none", marginTop: "0.75rem" }}>
          {items.map((item) => (
            <li
              key={item.id}
              style={{
                padding: "0.5rem 0",
                borderBottom: "1px solid #222",
                display: "flex",
                alignItems: "center",
                gap: "0.5rem",
              }}
            >
              <span style={{ fontSize: "1.1rem" }}>
                {item.done ? "✅" : "⬜"}
              </span>
              <span style={{ color: item.done ? "#888" : "#e0e0e0" }}>
                {item.name}
              </span>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
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
