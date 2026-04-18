import { Suspense } from "react";
import products from "../data/products.json";
import { ProductGrid } from "./product-grid";
import { Footer } from "./footer";
import { DebugBar } from "./debug-bar";

export const dynamic = "force-dynamic";

const dataSize = Math.round(JSON.stringify(products).length / 1024);

export default function Home() {
  return (
    <div className="min-h-screen">
      <DebugBar
        dataSize={dataSize}
        forcedDelay={100}
        extra={<span>Products: <span className="text-foreground">{products.length}</span></span>}
      />

      <main className="mx-auto max-w-7xl px-4 py-8">
        <header className="mb-8">
          <h1 className="text-2xl font-bold text-foreground">Storefront</h1>
          <p className="mt-1 text-sm text-muted">
            {products.length} products &middot; 100ms I/O delay &middot; Suspense streaming
          </p>
        </header>

        <Suspense
          fallback={
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
              {Array.from({ length: 20 }).map((_, i) => (
                <div key={i} className="animate-pulse rounded-lg border border-border bg-surface p-3">
                  <div className="mb-3 aspect-square rounded-md bg-border-light" />
                  <div className="mb-2 h-4 w-3/4 rounded bg-border-light" />
                  <div className="mb-3 h-3 w-1/2 rounded bg-border-light" />
                  <div className="flex justify-between">
                    <div className="h-4 w-16 rounded bg-border-light" />
                    <div className="h-3 w-12 rounded bg-border-light" />
                  </div>
                </div>
              ))}
            </div>
          }
        >
          <ProductGrid />
        </Suspense>
      </main>

      <Footer />
    </div>
  );
}
