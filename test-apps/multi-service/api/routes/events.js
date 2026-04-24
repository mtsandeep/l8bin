export async function registerEventRoutes(fastify) {
  const listeners = new Set();

  // Expose so result route can push
  fastify.decorate("sseBroadcast", (data) => {
    const msg = `event: leaderboard_update\ndata: ${JSON.stringify(data)}\n\n`;
    for (const res of listeners) {
      try {
        res.write(msg);
      } catch {
        listeners.delete(res);
      }
    }
  });

  fastify.get("/api/events", (req, reply) => {
    reply.raw.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    });

    listeners.add(reply.raw);
    req.raw.on("close", () => listeners.delete(reply.raw));

    // Keepalive
    const keepalive = setInterval(() => {
      try {
        reply.raw.write(":keepalive\n\n");
      } catch {
        clearInterval(keepalive);
        listeners.delete(reply.raw);
      }
    }, 15000);
  });
}
