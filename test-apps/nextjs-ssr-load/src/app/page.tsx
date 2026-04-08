// Heavy SSR page — generates 500 product cards server-side on every request
// No "use client" — this is a Server Component (App Router default)

export const dynamic = "force-dynamic";

const PRODUCTS = 500;
const CATEGORIES = ["Electronics", "Clothing", "Home", "Books", "Sports", "Food", "Toys", "Garden"];

function generateProducts() {
  const products = [];
  for (let i = 0; i < PRODUCTS; i++) {
    // Simulate DB query + computation per product
    const price = Math.round((Math.sin(i * 127.1) * 0.5 + 0.5) * 10000) / 100;
    const rating = Math.round((Math.cos(i * 311.7) * 0.5 + 0.5) * 50) / 10;
    const reviews = Math.floor(Math.abs(Math.sin(i * 73.3)) * 5000);
    const stock = Math.floor(Math.abs(Math.cos(i * 91.1)) * 200);
    const cat = CATEGORIES[i % CATEGORIES.length];
    const tags = [
      cat.toLowerCase(),
      `tag-${i % 50}`,
      stock < 10 ? "low-stock" : "in-stock",
      rating > 4 ? "top-rated" : "standard",
      price < 20 ? "budget" : price < 50 ? "mid-range" : "premium",
    ];
    const desc = `${cat} item #${i + 1}. `.repeat(3) + ` Premium quality ${cat.toLowerCase()} product with exceptional craftsmanship and attention to detail.`;

    products.push({
      id: i + 1,
      name: `${cat} Pro ${String(i + 1).padStart(3, "0")}`,
      price,
      originalPrice: Math.round(price * (1.1 + Math.sin(i) * 0.3) * 100) / 100,
      rating,
      reviews,
      stock,
      category: cat,
      tags,
      description: desc,
      sku: `SKU-${String(i + 1).padStart(6, "0")}`,
      weight: Math.round(Math.abs(Math.sin(i * 17.3)) * 5000) / 100,
      dimensions: `${Math.floor(Math.abs(Math.sin(i * 7.1)) * 100)}x${Math.floor(Math.abs(Math.cos(i * 7.1)) * 100)}x${Math.floor(Math.abs(Math.sin(i * 3.7)) * 50)} cm`,
    });
  }
  return products;
}

export default function Home() {
  const startTime = Date.now();
  const products = generateProducts();
  const renderTime = Date.now() - startTime;

  return (
    <div style={{ fontFamily: "system-ui, sans-serif", maxWidth: 1200, margin: "0 auto", padding: 20 }}>
      <header style={{ marginBottom: 32, borderBottom: "1px solid #e5e7eb", paddingBottom: 16 }}>
        <h1 style={{ fontSize: 24, margin: 0 }}>SSR Load Test Store</h1>
        <p style={{ color: "#6b7280", margin: "4px 0 0" }}>
          {PRODUCTS} products rendered server-side in {renderTime}ms
        </p>
      </header>

      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(200px, 1fr))", gap: 16 }}>
        {products.map((p) => (
          <a
            key={p.id}
            href={`/products/${p.id}`}
            style={{
              display: "block",
              border: "1px solid #e5e7eb",
              borderRadius: 8,
              padding: 12,
              textDecoration: "none",
              color: "inherit",
            }}
          >
            <div style={{ fontWeight: 600, fontSize: 14, marginBottom: 4 }}>
              {p.name}
            </div>
            <div style={{ fontSize: 12, color: "#6b7280", marginBottom: 4 }}>
              {p.category}
            </div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span style={{ fontWeight: 700, color: "#059669" }}>${p.price.toFixed(2)}</span>
              <span style={{ fontSize: 11, color: "#d97706" }}>{p.rating}/5 ({p.reviews})</span>
            </div>
            <div style={{ fontSize: 11, color: p.stock < 10 ? "#dc2626" : "#6b7280" }}>
              {p.stock < 10 ? `Only ${p.stock} left!` : `${p.stock} in stock`}
            </div>
          </a>
        ))}
      </div>

      <footer style={{ marginTop: 32, paddingTop: 16, borderTop: "1px solid #e5e7eb", color: "#9ca3af", fontSize: 12 }}>
        SSR Render Time: {renderTime}ms | Products: {PRODUCTS} | Server: {typeof window === "undefined" ? "yes" : "no"}
      </footer>
    </div>
  );
}
