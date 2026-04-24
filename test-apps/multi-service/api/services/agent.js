export async function agentPost(agentUrl, path, body) {
  const res = await fetch(`${agentUrl}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`Agent ${path} returned ${res.status}`);
  return res.json();
}
