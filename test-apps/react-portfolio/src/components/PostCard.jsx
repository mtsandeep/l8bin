import { Link } from "react-router-dom";

export default function PostCard({ post }) {
  return (
    <Link
      to={`/blog/${post.id}`}
      className="group block overflow-hidden rounded-xl border border-gray-800 bg-gray-900/50 transition-all hover:border-accent/40"
    >
      {post.image && (
        <div className="h-40 overflow-hidden">
          <img
            src={post.image}
            alt={post.title}
            className="h-full w-full object-cover transition-transform duration-500 group-hover:scale-105"
            loading="lazy"
          />
        </div>
      )}
      <div className="p-5">
        <div className="mb-2 flex items-center gap-3 text-xs text-gray-500">
          <time>{post.date}</time>
          <span className="flex gap-1.5">
            {post.tags.map((tag) => (
              <span
                key={tag}
                className="rounded bg-gray-800 px-1.5 py-0.5 text-gray-400"
              >
                {tag}
              </span>
            ))}
          </span>
        </div>
        <h3 className="mb-2 font-heading text-lg font-semibold text-gray-100 transition-colors group-hover:text-accent">
          {post.title}
        </h3>
        <p className="text-sm leading-relaxed text-gray-400">
          {post.excerpt}
        </p>
      </div>
    </Link>
  );
}
