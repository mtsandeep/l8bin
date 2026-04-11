import { useParams, Link, useNavigate } from "react-router-dom";

export default function PostDetail({ posts, onDelete }) {
  const { id } = useParams();
  const navigate = useNavigate();
  const post = posts.find((p) => p.id === id);

  if (!post) {
    return (
      <div className="mx-auto max-w-3xl px-4 py-16 text-center">
        <h1 className="mb-4 font-heading text-2xl font-bold text-gray-100">
          Entry not found
        </h1>
        <Link to="/blog" className="text-accent hover:underline">
          &larr; Back to Journal
        </Link>
      </div>
    );
  }

  const handleDelete = () => {
    if (window.confirm("Delete this entry?")) {
      onDelete(id);
      navigate("/blog");
    }
  };

  return (
    <div className="mx-auto max-w-3xl px-4 py-16">
      <Link
        to="/blog"
        className="mb-6 inline-block text-sm text-accent hover:underline"
      >
        &larr; Back to Journal
      </Link>

      <article>
        <header className="mb-8">
          <div className="mb-3 flex items-center gap-3 text-xs text-gray-500">
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
          <h1 className="font-heading text-3xl font-bold leading-tight text-gray-100">
            {post.title}
          </h1>
        </header>

        <div className="max-w-none space-y-4 text-gray-300">
          {post.body.split("\n\n").map((paragraph, i) => {
            if (paragraph.startsWith("**") && paragraph.endsWith("**")) {
              const text = paragraph.replace(/^\*\*|\*\*$/g, "");
              return (
                <h3
                  key={i}
                  className="font-heading text-lg font-semibold text-gray-100"
                >
                  {text}
                </h3>
              );
            }
            return (
              <p key={i} className="leading-relaxed">
                {paragraph}
              </p>
            );
          })}
        </div>

        <div className="mt-10 border-t border-gray-800 pt-6">
          <button
            onClick={handleDelete}
            className="text-sm text-red-400 transition-colors hover:text-red-300"
          >
            Delete this entry
          </button>
        </div>
      </article>
    </div>
  );
}
