"use client";

import { useState } from "react";
import { CategoryNav } from "./category-nav";

interface Product {
  id: number;
  name: string;
  price: number;
  originalPrice: number;
  rating: number;
  reviews: number;
  stock: number;
  category: string;
  image: string;
}

export function ProductListing({ products }: { products: Product[] }) {
  const [active, setActive] = useState("All");

  const filtered =
    active === "All" ? products : products.filter((p) => p.category === active);

  return (
    <>
      <CategoryNav
        categories={["All", "Electronics", "Fashion", "Home & Living", "Food & Kitchen", "Travel & Outdoors"]}
        active={active}
        onFilter={setActive}
      />
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
        {filtered.map((p) => (
          <a
            key={p.id}
            href={`/products/${p.id}`}
            className="group rounded-lg border border-border bg-surface transition-colors hover:border-border-light hover:bg-surface-hover"
          >
            <div className="relative aspect-square overflow-hidden rounded-t-lg bg-border">
              <img
                src={p.image}
                alt={p.name}
                loading="lazy"
                className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-105"
              />
              {p.stock < 10 && (
                <span className="absolute left-2 top-2 rounded bg-danger/90 px-2 py-0.5 text-xs font-medium text-white">
                  Low Stock
                </span>
              )}
            </div>

            <div className="p-3">
              <p className="mb-1 text-xs text-muted">{p.category}</p>
              <h3 className="mb-2 line-clamp-1 text-sm font-semibold text-foreground">
                {p.name}
              </h3>

              <div className="flex items-center justify-between">
                <div className="flex items-baseline gap-1.5">
                  <span className="text-sm font-bold text-success">
                    ${p.price.toFixed(2)}
                  </span>
                  {p.originalPrice > p.price && (
                    <span className="text-xs text-muted line-through">
                      ${p.originalPrice.toFixed(2)}
                    </span>
                  )}
                </div>
                <span className="flex items-center gap-0.5 text-xs text-warning">
                  ★ {p.rating}
                </span>
              </div>

              <p className="mt-1.5 text-xs text-muted">
                {p.reviews.toLocaleString()} reviews
              </p>
            </div>
          </a>
        ))}
      </div>
    </>
  );
}
