"use client";

const CATEGORY_ICONS: Record<string, string> = {
  All: "🛍️",
  Electronics: "💻",
  Fashion: "👗",
  "Home & Living": "🏠",
  "Food & Kitchen": "🍳",
  "Travel & Outdoors": "🏕️",
};

export function CategoryNav({
  categories,
  active,
  onFilter,
}: {
  categories: string[];
  active: string;
  onFilter: (cat: string) => void;
}) {
  return (
    <nav className="mb-6 flex flex-wrap gap-2">
      {categories.map((cat) => (
        <button
          key={cat}
          onClick={() => onFilter(cat)}
          className={`rounded-full px-4 py-1.5 text-sm font-medium transition-colors ${
            active === cat
              ? "bg-accent text-white"
              : "bg-surface text-muted hover:bg-surface-hover hover:text-foreground"
          }`}
        >
          <span className="mr-1.5">{CATEGORY_ICONS[cat]}</span>
          {cat}
        </button>
      ))}
    </nav>
  );
}
