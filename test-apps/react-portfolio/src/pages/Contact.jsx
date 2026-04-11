import { useState } from "react";

export default function Contact() {
  const [form, setForm] = useState({ name: "", email: "", message: "" });
  const [errors, setErrors] = useState({});
  const [submitted, setSubmitted] = useState(false);

  const validate = () => {
    const e = {};
    if (!form.name.trim()) e.name = "Name is required";
    if (!form.email.trim()) e.email = "Email is required";
    else if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(form.email))
      e.email = "Invalid email address";
    if (!form.message.trim()) e.message = "Message is required";
    setErrors(e);
    return Object.keys(e).length === 0;
  };

  const handleSubmit = (e) => {
    e.preventDefault();
    if (!validate()) return;
    setSubmitted(true);
  };

  const update = (field) => (e) =>
    setForm((prev) => ({ ...prev, [field]: e.target.value }));

  if (submitted) {
    return (
      <div className="mx-auto max-w-2xl px-4 py-16 text-center">
        <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-green-500/10 text-green-400">
          <svg className="h-8 w-8" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
          </svg>
        </div>
        <h1 className="mb-2 font-heading text-2xl font-bold text-gray-100">
          Message Sent!
        </h1>
        <p className="mb-6 text-gray-400">
          Thanks for reaching out. We'll get back to you soon.
        </p>
        <button
          onClick={() => {
            setForm({ name: "", email: "", message: "" });
            setSubmitted(false);
          }}
          className="rounded-lg border border-gray-700 px-4 py-2 text-sm text-gray-300 transition-colors hover:border-gray-500"
        >
          Send another message
        </button>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-2xl px-4 py-16">
      <h1 className="mb-2 font-heading text-3xl font-bold text-gray-100">
        Get in Touch
      </h1>
      <p className="mb-8 text-gray-400">
        Have a question or want to work together? Drop us a message.
      </p>

      <form onSubmit={handleSubmit} className="space-y-5">
        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Name
          </label>
          <input
            type="text"
            value={form.name}
            onChange={update("name")}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="Your name"
          />
          {errors.name && (
            <p className="mt-1 text-xs text-red-400">{errors.name}</p>
          )}
        </div>

        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Email
          </label>
          <input
            type="email"
            value={form.email}
            onChange={update("email")}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="you@example.com"
          />
          {errors.email && (
            <p className="mt-1 text-xs text-red-400">{errors.email}</p>
          )}
        </div>

        <div>
          <label className="mb-1.5 block text-sm font-medium text-gray-300">
            Message
          </label>
          <textarea
            value={form.message}
            onChange={update("message")}
            rows={6}
            className="w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2.5 text-sm leading-relaxed text-gray-100 placeholder-gray-500 outline-none transition-colors focus:border-accent"
            placeholder="What's on your mind?"
          />
          {errors.message && (
            <p className="mt-1 text-xs text-red-400">{errors.message}</p>
          )}
        </div>

        <button
          type="submit"
          className="rounded-lg bg-accent px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-light"
        >
          Send Message
        </button>
      </form>
    </div>
  );
}
