import { ProductListing } from "./product-listing";
import products from "../data/products.json";

const FORCED_DELAY = 100;

export async function ProductGrid() {
  // Artificial I/O delay — inside Suspense so shell streams first
  await new Promise((resolve) => setTimeout(resolve, FORCED_DELAY));

  const dataSize = Math.round(JSON.stringify(products).length / 1024);

  return (
    <>
      <div className="mb-4 flex items-center gap-3 text-xs text-muted">
        <span>
          Data: <span className="text-foreground">~{dataSize}KB</span>
        </span>
      </div>

      <ProductListing products={products} />
    </>
  );
}
