import { useState, useMemo } from "react";
import { collections } from "../data/seed";
import ProjectCard from "../components/ProjectCard";

export default function Projects() {
  const [search, setSearch] = useState("");
  const [activeTag, setActiveTag] = useState(null);

  const allTags = useMemo(
    () => [...new Set(collections.flatMap((p) => p.tags))].sort(),
    []
  );

  const filtered = useMemo(() => {
    return collections.filter((p) => {
      const matchesSearch =
        !search ||
        p.title.toLowerCase().includes(search.toLowerCase()) ||
        p.description.toLowerCase().includes(search.toLowerCase());
      const matchesTag = !activeTag || p.tags.includes(activeTag);
      return matchesSearch && matchesTag;
    });
  }, [search, activeTag]);

  return (
    <div className="mx-auto max-w-5xl px-4 py-16">
      <h1 className="mb-2 font-heading text-3xl font-bold text-gray-100">
        Collections
      </h1>
      <p className="mb-8 text-gray-400">
        Handcrafted textiles sourced directly from artisan families across India.
      </p>

      <div className="mb-6 space-y-3">
        <input
          type="text"
          placeholder="Search collections..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
        />
        <div className="flex flex-wrap gap-2">
          <button
            onClick={() => setActiveTag(null)}
            className={`rounded-md px-3 py-1 text-xs font-medium transition-colors ${
              !activeTag
                ? "bg-accent text-white"
                : "bg-gray-800 text-gray-400 hover:text-gray-200"
            }`}
          >
            All
          </button>
          {allTags.map((tag) => (
            <button
              key={tag}
              onClick={() => setActiveTag(activeTag === tag ? null : tag)}
              className={`rounded-md px-3 py-1 text-xs font-medium transition-colors ${
                activeTag === tag
                  ? "bg-accent text-white"
                  : "bg-gray-800 text-gray-400 hover:text-gray-200"
              }`}
            >
              {tag}
            </button>
          ))}
        </div>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        {filtered.map((c) => (
          <ProjectCard key={c.id} project={c} />
        ))}
      </div>

      {filtered.length === 0 && (
        <p className="py-12 text-center text-gray-500">
          No collections match your filters.
        </p>
      )}
    </div>
  );
}
