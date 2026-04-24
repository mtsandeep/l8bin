export async function fetchQuiz(level = 1, sessionId = null, name = null) {
  const params = new URLSearchParams({ level: String(level) });
  if (sessionId) params.set("sessionId", sessionId);
  if (name) params.set("name", name);
  const res = await fetch(`/api/quiz?${params}`);
  if (!res.ok) throw new Error("Failed to fetch quiz");
  return res.json();
}

export async function submitLevelResult(sessionId, answers, level) {
  const res = await fetch("/api/result", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ sessionId, answers, level }),
  });
  if (!res.ok) throw new Error("Failed to submit result");
  return res.json();
}

export async function finishGame(sessionId, name) {
  const res = await fetch("/api/finish", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ sessionId, name }),
  });
  if (!res.ok) throw new Error("Failed to finalize game");
  return res.json();
}

export async function fetchLeaderboard() {
  const res = await fetch("/api/leaderboard");
  if (!res.ok) throw new Error("Failed to fetch leaderboard");
  return res.json();
}

export function subscribeLeaderboard(onUpdate) {
  const es = new EventSource("/api/events");
  es.addEventListener("leaderboard_update", (e) => {
    try {
      onUpdate(JSON.parse(e.data));
    } catch {}
  });
  return () => es.close();
}

export async function fetchStats() {
  const res = await fetch("/api/stats");
  if (!res.ok) throw new Error("Failed to fetch stats");
  return res.json();
}
