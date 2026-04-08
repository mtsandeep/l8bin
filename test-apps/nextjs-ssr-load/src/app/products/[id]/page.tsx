// Heavy SSR product detail page — renders product + related items + reviews
// Server Component by default in App Router

const CATEGORIES = ["Electronics", "Clothing", "Home", "Books", "Sports", "Food", "Toys", "Garden"];

function generateProduct(id: number) {
  const i = id - 1;
  const price = Math.round((Math.sin(i * 127.1) * 0.5 + 0.5) * 10000) / 100;
  const rating = Math.round((Math.cos(i * 311.7) * 0.5 + 0.5) * 50) / 10;
  const reviews = Math.floor(Math.abs(Math.sin(i * 73.3)) * 5000);
  const stock = Math.floor(Math.abs(Math.cos(i * 91.1)) * 200);
  const cat = CATEGORIES[i % CATEGORIES.length];

  return {
    id,
    name: `${cat} Pro ${String(id).padStart(3, "0")}`,
    price,
    rating,
    reviews,
    stock,
    category: cat,
    sku: `SKU-${String(id).padStart(6, "0")}`,
    weight: `${Math.round(Math.abs(Math.sin(i * 17.3)) * 5000) / 100} kg`,
    description: `${cat} item #${id}. Premium quality ${cat.toLowerCase()} product with exceptional craftsmanship and attention to detail. Perfect for everyday use and special occasions alike. Made with the finest materials sourced from around the world.`,
  };
}

function generateReviews(productId: number) {
  const reviews = [];
  for (let i = 0; i < 20; i++) {
    const seed = productId * 100 + i;
    reviews.push({
      id: i + 1,
      user: `user_${Math.floor(Math.abs(Math.sin(seed * 3.7)) * 10000)}`,
      rating: Math.round((Math.sin(seed * 11.3) * 0.5 + 0.5) * 50) / 10,
      title: `Review ${i + 1} for product ${productId}`,
      body: "Great product! Exceeded my expectations in every way. Would definitely recommend to friends and family. ".repeat(2),
      date: new Date(Date.now() - Math.floor(Math.abs(Math.sin(seed * 7.1)) * 365 * 86400000)).toISOString().split("T")[0],
    });
  }
  return reviews;
}

function generateRelated(productId: number) {
  const related = [];
  for (let i = 0; i < 8; i++) {
    const rid = ((productId - 1 + (i + 1) * 37) % 500) + 1;
    related.push(generateProduct(rid));
  }
  return related;
}

export default async function ProductPage({ params }: { params: Promise<{ id: string }> }) {
  const { id: idStr } = await params;
  const id = parseInt(idStr, 10) || 1;

  const product = generateProduct(id);
  const reviews = generateReviews(id);
  const related = generateRelated(id);

  return (
    <div style={{ fontFamily: "system-ui, sans-serif", maxWidth: 900, margin: "0 auto", padding: 20 }}>
      <a href="/" style={{ color: "#6366f1", fontSize: 14 }}>← Back to all products</a>

      <div style={{ marginTop: 20, display: "flex", gap: 32 }}>
        <div style={{ flex: 1 }}>
          <div style={{ width: "100%", height: 300, background: "linear-gradient(135deg, #667eea 0%, #764ba2 100%)", borderRadius: 8, display: "flex", alignItems: "center", justifyContent: "center", color: "#fff", fontSize: 48, fontWeight: 700 }}>
            {product.name}
          </div>
        </div>
        <div style={{ flex: 1 }}>
          <span style={{ fontSize: 12, color: "#6b7280" }}>{product.category}</span>
          <h1 style={{ fontSize: 28, margin: "4px 0" }}>{product.name}</h1>
          <div style={{ display: "flex", alignItems: "center", gap: 8, margin: "8px 0" }}>
            <span style={{ color: "#d97706" }}>{product.rating}/5</span>
            <span style={{ color: "#6b7280" }}>({product.reviews} reviews)</span>
          </div>
          <div style={{ fontSize: 32, fontWeight: 700, color: "#059669" }}>${product.price.toFixed(2)}</div>
          <div style={{ margin: "8px 0", color: product.stock < 10 ? "#dc2626" : "#059669", fontWeight: 600 }}>
            {product.stock < 10 ? `Only ${product.stock} left!` : `${product.stock} in stock`}
          </div>
          <div style={{ fontSize: 13, color: "#6b7280", lineHeight: 1.6, margin: "16px 0" }}>
            {product.description}
          </div>
          <div style={{ fontSize: 12, color: "#9ca3af" }}>SKU: {product.sku} | Weight: {product.weight}</div>
        </div>
      </div>

      <section style={{ marginTop: 32 }}>
        <h2 style={{ fontSize: 20, borderBottom: "1px solid #e5e7eb", paddingBottom: 8 }}>Reviews ({reviews.length})</h2>
        {reviews.map((r) => (
          <div key={r.id} style={{ border: "1px solid #e5e7eb", borderRadius: 6, padding: 12, margin: "8px 0" }}>
            <div style={{ display: "flex", justifyContent: "space-between" }}>
              <strong>{r.title}</strong>
              <span style={{ color: "#d97706" }}>{r.rating}/5</span>
            </div>
            <div style={{ fontSize: 12, color: "#6b7280", margin: "4px 0" }}>by {r.user} on {r.date}</div>
            <p style={{ fontSize: 13, color: "#374151", margin: "4px 0" }}>{r.body}</p>
          </div>
        ))}
      </section>

      <section style={{ marginTop: 32 }}>
        <h2 style={{ fontSize: 20, borderBottom: "1px solid #e5e7eb", paddingBottom: 8 }}>Related Products</h2>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))", gap: 12, marginTop: 12 }}>
          {related.map((p) => (
            <a key={p.id} href={`/products/${p.id}`} style={{ border: "1px solid #e5e7eb", borderRadius: 6, padding: 8, textDecoration: "none", color: "inherit" }}>
              <div style={{ fontWeight: 600, fontSize: 13 }}>{p.name}</div>
              <div style={{ color: "#059669", fontWeight: 700 }}>${p.price.toFixed(2)}</div>
              <div style={{ fontSize: 11, color: "#d97706" }}>{p.rating}/5</div>
            </a>
          ))}
        </div>
      </section>
    </div>
  );
}
