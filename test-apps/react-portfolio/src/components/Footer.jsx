import { profile } from "../data/seed";

export default function Footer() {
  return (
    <footer className="border-t border-gray-800 py-8 text-center text-sm text-gray-500">
      <div className="mx-auto max-w-5xl px-4">
        <div className="mb-3 flex justify-center gap-4">
          <a
            href={`https://instagram.com/${profile.instagram}`}
            target="_blank"
            rel="noopener noreferrer"
            className="transition-colors hover:text-accent"
          >
            Instagram
          </a>
          <a
            href={`https://youtube.com/@${profile.youtube}`}
            target="_blank"
            rel="noopener noreferrer"
            className="transition-colors hover:text-accent"
          >
            YouTube
          </a>
          <a
            href={`https://twitter.com/${profile.twitter}`}
            target="_blank"
            rel="noopener noreferrer"
            className="transition-colors hover:text-accent"
          >
            Twitter
          </a>
          <a href={`mailto:${profile.email}`} className="transition-colors hover:text-accent">
            Email
          </a>
        </div>
        <p className="font-mono text-xs">
          Handcrafted with care. &copy; {new Date().getFullYear()} SUTRA
        </p>
      </div>
    </footer>
  );
}
