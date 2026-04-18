const REVIEW_BODIES = [
  "Absolutely love this product! Exceeded all my expectations. The quality is outstanding and it arrived earlier than expected. Would definitely recommend to anyone looking for a reliable option.",
  "Great value for the price. Solid build quality and performs exactly as described. The packaging was excellent too — no damage at all during shipping. Five stars from me!",
  "Been using this for a few weeks now and it's become an essential part of my daily routine. Very intuitive to use and the design is sleek. Minor gripe: the instructions could be clearer.",
  "Purchased this as a gift and the recipient was thrilled. Premium feel and looks even better in person than in the photos. Customer service was helpful when I had a question about warranty.",
  "Decent product overall. Works well for the most part but I knocked off a star because the color was slightly different from what was shown online. Still happy with the purchase though.",
  "This is my second time buying from this brand and they consistently deliver. The attention to detail is impressive. Feels like a premium product without the premium price tag.",
  "Perfect for everyday use. Lightweight yet durable. I've put it through quite a bit of wear and tear and it still looks brand new. The materials are top-notch.",
  "Was skeptical at first given the price point, but this completely won me over. Performance is on par with products that cost twice as much. Highly satisfied with this purchase.",
];

const REVIEWERS = [
  "alex_dev", "sarah_m", "mike_reviews", "tech_sam", "jenny_k",
  "david_chen", "emma_w", "raj_patel", "lisa_m", "carlos_r",
  "nina_t", "oliver_j", "priya_s", "tom_w", "maria_g",
  "james_h", "yuki_t", "anna_k", "ben_l", "sofia_r",
];

export async function ReviewSection({ productId }: { productId: number }) {
  const reviews = [];
  for (let i = 0; i < 15; i++) {
    const seed = productId * 100 + i;
    reviews.push({
      id: i + 1,
      user: REVIEWERS[(productId + i) % REVIEWERS.length],
      rating: Math.max(3, Math.min(5, Math.round((Math.sin(seed * 11.3) * 0.5 + 0.7) * 50) / 10)),
      title: [
        "Excellent quality!", "Great purchase", "Worth every penny",
        "Highly recommend", "Solid product", "Good value", "Love it!",
        "Exceeded expectations", "Very impressed", "A must-have",
        "Top tier quality", "Really good", "Fantastic buy", "Surprisingly great", "No regrets",
      ][i],
      body: REVIEW_BODIES[(productId + i) % REVIEW_BODIES.length],
      date: new Date(
        Date.now() - Math.floor(Math.abs(Math.sin(seed * 7.1)) * 365) * 86400000
      )
        .toISOString()
        .split("T")[0],
      helpful: Math.floor(Math.abs(Math.sin(seed * 3.3)) * 50),
    });
  }

  return (
    <section>
      <h2 className="mb-4 border-b border-border pb-2 text-lg font-bold text-foreground">
        Reviews ({reviews.length})
      </h2>
      <div className="space-y-3">
        {reviews.map((r) => (
          <div
            key={r.id}
            className="rounded-lg border border-border bg-surface p-4"
          >
            <div className="flex items-center justify-between">
              <div>
                <span className="font-semibold text-foreground">
                  {r.title}
                </span>
                <div className="mt-0.5 flex items-center gap-2 text-xs text-muted">
                  <span className="text-warning">★ {r.rating}</span>
                  <span>by {r.user}</span>
                  <span>{r.date}</span>
                </div>
              </div>
              {r.helpful > 20 && (
                <span className="rounded bg-success/10 px-2 py-0.5 text-xs text-success">
                  {r.helpful} found helpful
                </span>
              )}
            </div>
            <p className="mt-2 text-sm leading-relaxed text-muted">{r.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
