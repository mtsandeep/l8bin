export function Footer() {
  return (
    <footer className="mt-16 border-t border-border bg-surface/50">
      <div className="mx-auto flex max-w-7xl flex-col items-center gap-2 px-4 py-8 text-center text-xs text-muted">
        <p className="font-semibold text-foreground/60">
          Storefront &mdash; SSR Load Test
        </p>
        <p>Next.js 16 &middot; React Server Components &middot; Tailwind CSS</p>
        <p>
          Built for benchmarking VPS performance under realistic SSR workloads.
        </p>
        <p>
          Images by{" "}
          <a
            href="https://unsplash.com"
            target="_blank"
            rel="noopener noreferrer"
            className="text-accent hover:underline"
          >
            Unsplash
          </a>
        </p>
      </div>
    </footer>
  );
}
