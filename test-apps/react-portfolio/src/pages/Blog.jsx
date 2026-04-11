import { Link } from "react-router-dom";
import PostCard from "../components/PostCard";

export default function Blog({ posts }) {
  return (
    <div className="mx-auto max-w-5xl px-4 py-16">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="font-heading text-3xl font-bold text-gray-100">
            Journal
          </h1>
          <p className="mt-1 text-gray-400">
            Stories from the loom, the artisans, and the journey.
          </p>
        </div>
        <Link
          to="/blog/new"
          className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-light"
        >
          + New Entry
        </Link>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        {posts.map((post) => (
          <PostCard key={post.id} post={post} />
        ))}
      </div>

      {posts.length === 0 && (
        <p className="py-12 text-center text-gray-500">
          No journal entries yet. Write your first one!
        </p>
      )}
    </div>
  );
}
