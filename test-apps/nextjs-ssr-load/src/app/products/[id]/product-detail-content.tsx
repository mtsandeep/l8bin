import Link from "next/link";
import products from "../../../data/products.json";
import { ReviewSection } from "./review-section";

const FORCED_DELAY = 100;

function getProduct(id: number) {
  return products.find((p) => p.id === id);
}

function getRelated(productId: number, category: string) {
  return products
    .filter((p) => p.category === category && p.id !== productId)
    .slice(0, 8);
}

export async function ProductDetailContent({ id }: { id: number }) {
  // Artificial I/O delay — inside Suspense so shell streams first
  await new Promise((resolve) => setTimeout(resolve, FORCED_DELAY));

  const product = getProduct(id);
  if (!product) return null;

  const related = getRelated(id, product.category);
  const discount = Math.round(
    ((product.originalPrice - product.price) / product.originalPrice) * 100
  );
  const dataSize = Math.round(JSON.stringify({ product, related }).length / 1024);

  return (
    <>
      <main className="mx-auto max-w-7xl px-4 py-8">
        <Link
          href="/"
          className="mb-6 inline-flex items-center gap-1 text-sm text-accent hover:underline"
        >
          ← Back to products
        </Link>

        {/* Product Hero */}
        <div className="mt-4 grid gap-8 md:grid-cols-2">
          {/* Image */}
          <div className="overflow-hidden rounded-lg border border-border bg-surface">
            <img
              src={product.image}
              alt={product.name}
              className="aspect-square w-full object-cover"
            />
          </div>

          {/* Info */}
          <div>
            <span className="inline-block rounded-full bg-accent/10 px-3 py-0.5 text-xs font-medium text-accent">
              {product.category}
            </span>

            <h1 className="mt-3 text-2xl font-bold text-foreground">
              {product.name}
            </h1>

            <div className="mt-2 flex items-center gap-2">
              <span className="text-sm text-warning">★ {product.rating}</span>
              <span className="text-sm text-muted">
                ({product.reviews.toLocaleString()} reviews)
              </span>
            </div>

            <div className="mt-4 flex items-baseline gap-3">
              <span className="text-3xl font-bold text-success">
                ${product.price.toFixed(2)}
              </span>
              {discount > 0 && (
                <>
                  <span className="text-lg text-muted line-through">
                    ${product.originalPrice.toFixed(2)}
                  </span>
                  <span className="rounded bg-danger/10 px-2 py-0.5 text-sm font-medium text-danger">
                    -{discount}%
                  </span>
                </>
              )}
            </div>

            <p
              className={`mt-3 text-sm font-semibold ${
                product.stock < 10 ? "text-danger" : "text-success"
              }`}
            >
              {product.stock < 10
                ? `Only ${product.stock} left in stock!`
                : `${product.stock} in stock`}
            </p>

            <p className="mt-4 text-sm leading-relaxed text-muted">
              {product.description}
            </p>

            <div className="mt-4 flex flex-wrap gap-1.5">
              {product.tags.map((tag) => (
                <span
                  key={tag}
                  className="rounded-full border border-border px-2.5 py-0.5 text-xs text-muted"
                >
                  {tag}
                </span>
              ))}
            </div>

            <div className="mt-6 space-y-2 border-t border-border pt-4 text-xs text-muted">
              <p>
                SKU: <span className="text-foreground">{product.sku}</span>
              </p>
              <p>
                Weight: <span className="text-foreground">{product.weight}</span>
              </p>
              <p>
                Dimensions:{" "}
                <span className="text-foreground">{product.dimensions}</span>
              </p>
            </div>
          </div>
        </div>

        {/* Reviews + Related Products — 2 column layout */}
        <div className="mt-10 grid gap-8 lg:grid-cols-3">
          {/* Reviews — 2/3 width */}
          <div className="lg:col-span-2">
            <ReviewSection productId={id} />
          </div>

          {/* Related Products — 1/3 width, sticky */}
          {related.length > 0 && (
            <aside className="lg:col-span-1">
              <h2 className="mb-4 border-b border-border pb-2 text-lg font-bold text-foreground">
                Related Products
              </h2>
              <div className="sticky top-12 space-y-3">
                {related.map((p) => (
                  <Link
                    key={p.id}
                    href={`/products/${p.id}`}
                    className="group flex gap-3 rounded-lg border border-border bg-surface p-3 transition-colors hover:border-border-light hover:bg-surface-hover"
                  >
                    <div className="h-16 w-16 flex-shrink-0 overflow-hidden rounded-md bg-border">
                      <img
                        src={p.image}
                        alt={p.name}
                        loading="lazy"
                        className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-105"
                      />
                    </div>
                    <div className="min-w-0">
                      <p className="line-clamp-1 text-sm font-semibold text-foreground">
                        {p.name}
                      </p>
                      <p className="mt-0.5 text-xs text-muted">{p.category}</p>
                      <p className="mt-1 text-sm font-bold text-success">
                        ${p.price.toFixed(2)}
                      </p>
                    </div>
                  </Link>
                ))}
              </div>
            </aside>
          )}
        </div>
      </main>
    </>
  );
}
