import { Suspense } from "react";
import { notFound } from "next/navigation";
import products from "../../../data/products.json";
import { Footer } from "../../footer";
import { DebugBar } from "../../debug-bar";
import { ProductDetailContent } from "./product-detail-content";

export const dynamic = "force-dynamic";

export default async function ProductPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id: idStr } = await params;
  const id = parseInt(idStr, 10) || 1;
  const product = products.find((p) => p.id === id);

  if (!product) return notFound();

  const related = products
    .filter((p) => p.category === product.category && p.id !== id)
    .slice(0, 8);
  const dataSize = Math.round(JSON.stringify({ product, related }).length / 1024);

  return (
    <div className="min-h-screen">
      <DebugBar
        dataSize={dataSize}
        forcedDelay={100}
        extra={<span>ID: <span className="text-foreground">{id}</span></span>}
      />

      <Suspense
        fallback={
          <div className="mx-auto max-w-7xl px-4 py-8">
            <div className="mb-6 h-4 w-32 rounded bg-border-light" />
            <div className="mt-4 grid gap-8 md:grid-cols-2">
              <div className="aspect-square animate-pulse rounded-lg border border-border bg-surface">
                <div className="h-full w-full rounded-lg bg-border-light" />
              </div>
              <div className="space-y-3">
                <div className="h-5 w-24 rounded-full bg-border-light" />
                <div className="h-7 w-3/4 rounded bg-border-light" />
                <div className="h-4 w-1/3 rounded bg-border-light" />
                <div className="h-8 w-24 rounded bg-border-light" />
                <div className="h-4 w-40 rounded bg-border-light" />
                <div className="h-20 w-full rounded bg-border-light" />
                <div className="h-4 w-1/2 rounded bg-border-light" />
              </div>
            </div>
          </div>
        }
      >
        <ProductDetailContent id={id} />
      </Suspense>

      <Footer />
    </div>
  );
}
