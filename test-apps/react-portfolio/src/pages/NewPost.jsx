import { useState } from "react";
import { useNavigate, Link } from "react-router-dom";

export default function NewPost({ onAdd }) {
  const navigate = useNavigate();
  const [form, setForm] = useState({
    title: "",
    excerpt: "",
    body: "",
    tags: "",
  });
  const [errors, setErrors] = useState({});

  const validate = () => {
    const e = {};
    if (!form.title.trim()) e.title = "Title is required";
    if (!form.excerpt.trim()) e.excerpt = "Excerpt is required";
    if (!form.body.trim()) e.body = "Body is required";
    setErrors(e);
    return Object.keys(e).length === 0;
  };

  const handleSubmit = (e) => {
    e.preventDefault();
    if (!validate()) return;
    onAdd({
      ...form,
      tags: form.tags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
    });
    navigate("/blog");
  };

  const update = (field) => (e) =>
    setForm((prev) => ({ ...prev, [field]: e.target.value }));

  return (
    <div className="mx-auto max-w-3xl px-4 py-16">
      <Link
        to="/blog"
        className="mb-6 inline-block text-sm text-accent hover:underline"
      >
        &larr; Back to Journal
      </Link>

      <h1 className="mb-8 font-heading text-3xl font-bold text-gray-100">
        New Entry
      </h1>

      <form onSubmit={handleSubmit} className="space-y-5">
        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Title
          </label>
          <input
            type="text"
            value={form.title}
            onChange={update("title")}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="My Visit to the Weavers of..."
          />
          {errors.title && (
            <p className="mt-1 text-xs text-red-400">{errors.title}</p>
          )}
        </div>

        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Excerpt
          </label>
          <input
            type="text"
            value={form.excerpt}
            onChange={update("excerpt")}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="A short summary of your entry..."
          />
          {errors.excerpt && (
            <p className="mt-1 text-xs text-red-400">{errors.excerpt}</p>
          )}
        </div>

        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Body
          </label>
          <textarea
            value={form.body}
            onChange={update("body")}
            rows={12}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm leading-relaxed text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="Write your journal entry here..."
          />
          {errors.body && (
            <p className="mt-1 text-xs text-red-400">{errors.body}</p>
          )}
        </div>

        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Tags (comma-separated)
          </label>
          <input
            type="text"
            value={form.tags}
            onChange={update("tags")}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="Handloom, Block Print, Jaipur"
          />
        </div>

        <button
          type="submit"
          className="rounded-lg bg-accent px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-light"
        >
          Publish Entry
        </button>
      </form>
    </div>
  );
}
