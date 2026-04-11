import { useState, useEffect } from "react";
import { Link } from "react-router-dom";
import { profile, collections } from "../data/seed";
import PostCard from "../components/PostCard";

const roles = [
  "Fashion Stylist",
  "Textile Curator",
  "Content Creator",
  "Artisan Advocate",
  "Handloom Enthusiast",
];

const heroImages = [
  "https://images.unsplash.com/photo-1620799140408-edc6dcb6d633?w=1200&h=600&fit=crop&q=80",
  "https://images.unsplash.com/photo-1528459105426-b9548367069b?w=1200&h=600&fit=crop&q=80",
  "https://images.unsplash.com/photo-1605001011156-cbf0b0f67a51?w=1200&h=600&fit=crop&q=80",
];

export default function Home({ posts }) {
  const [roleIndex, setRoleIndex] = useState(0);
  const [heroImg, setHeroImg] = useState(0);
  const featured = collections.filter((p) => p.featured);
  const recentPosts = posts.slice(0, 2);

  useEffect(() => {
    const ri = setInterval(() => setRoleIndex((i) => (i + 1) % roles.length), 3000);
    const hi = setInterval(() => setHeroImg((i) => (i + 1) % heroImages.length), 5000);
    return () => { clearInterval(ri); clearInterval(hi); };
  }, []);

  return (
    <div>
      {/* Hero */}
      <section className="relative h-[70vh] min-h-[480px] overflow-hidden">
        {heroImages.map((src, i) => (
          <div
            key={src}
            className="absolute inset-0 bg-cover bg-center transition-opacity duration-1000"
            style={{
              backgroundImage: `url(${src})`,
              opacity: i === heroImg ? 1 : 0,
            }}
          />
        ))}
        <div className="absolute inset-0 bg-gradient-to-t from-gray-950 via-gray-950/70 to-gray-950/30" />

        <div className="relative mx-auto flex h-full max-w-5xl flex-col justify-end px-4 pb-16 animate-fade-up">
          <div className="mb-2 inline-flex w-fit items-center gap-2 rounded-full border border-accent/30 bg-accent/10 px-3 py-1 text-xs font-medium text-accent">
            <span className="h-1.5 w-1.5 rounded-full bg-accent animate-pulse" />
            Now shipping across India
          </div>
          <h1 className="font-heading text-4xl font-bold tracking-tight text-white sm:text-6xl">
            Hi, I'm {profile.name.split(" ")[0]}.
          </h1>
          <div className="mt-2 flex items-center gap-2">
            <span className="text-xl text-gray-300 sm:text-2xl">I'm a</span>
            <span className="cursor-blink font-heading text-xl font-semibold text-accent sm:text-2xl">
              {roles[roleIndex]}
            </span>
          </div>
          <p className="mt-4 max-w-xl text-base leading-relaxed text-gray-300 sm:text-lg">
            {profile.tagline}
          </p>
          <div className="mt-6 flex gap-3">
            <Link
              to="/projects"
              className="rounded-lg bg-accent px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-light"
            >
              View Collections
            </Link>
            <Link
              to="/about"
              className="rounded-lg border border-white/20 bg-white/10 px-5 py-2.5 text-sm font-semibold text-white backdrop-blur-sm transition-colors hover:bg-white/20"
            >
              Our Story
            </Link>
          </div>

          <div className="mt-6 flex gap-1.5">
            {heroImages.map((_, i) => (
              <button
                key={i}
                onClick={() => setHeroImg(i)}
                className={`h-1.5 rounded-full transition-all ${
                  i === heroImg ? "w-6 bg-accent" : "w-1.5 bg-white/30"
                }`}
                aria-label={`Slide ${i + 1}`}
              />
            ))}
          </div>
        </div>
      </section>

      <div className="mx-auto max-w-5xl px-4">
        {/* Stats */}
        <section className="-mt-10 mb-16 relative z-10 grid grid-cols-2 gap-3 sm:grid-cols-4">
          {[
            { label: "Collections", value: collections.length },
            { label: "Journal Entries", value: posts.length },
            { label: "Pieces Sold", value: collections.reduce((a, p) => a + p.stars, 0).toLocaleString() },
            { label: "Artisan Families", value: "80+" },
          ].map(({ label, value }) => (
            <div
              key={label}
              className="rounded-xl border border-gray-800 bg-gray-900/80 p-4 text-center backdrop-blur-sm"
            >
              <div className="font-heading text-2xl font-bold text-accent">
                {value}
              </div>
              <div className="mt-1 text-xs text-gray-500">{label}</div>
            </div>
          ))}
        </section>

        {/* From Loom to You */}
        <section className="mb-20">
          <h2 className="mb-8 text-center font-heading text-2xl font-bold text-gray-100">
            From Loom to You
          </h2>
          <div className="grid gap-6 sm:grid-cols-3">
            {[
              {
                img: "https://images.unsplash.com/photo-1558171813-4c088753af8f?w=400&h=300&fit=crop&q=80",
                title: "We Source",
                desc: "We visit handloom clusters across India and work directly with artisan families.",
              },
              {
                img: "https://images.unsplash.com/photo-1609505848912-b7c3b8b4beda?w=400&h=300&fit=crop&q=80",
                title: "You Style",
                desc: "Browse collections styled for modern living. Every piece photographed in natural light.",
              },
              {
                img: "https://images.unsplash.com/photo-1595341888016-a392ef81b7de?w=400&h=300&fit=crop&q=80",
                title: "They Thrive",
                desc: "40% of every sale goes directly to the weaver family. No middlemen. No markups.",
              },
            ].map(({ img, title, desc }) => (
              <div key={title} className="group overflow-hidden rounded-xl border border-gray-800 bg-gray-900/50">
                <div className="h-48 overflow-hidden">
                  <img
                    src={img}
                    alt={title}
                    className="h-full w-full object-cover transition-transform duration-500 group-hover:scale-105"
                    loading="lazy"
                  />
                </div>
                <div className="p-5">
                  <h3 className="font-heading text-lg font-semibold text-accent">
                    {title}
                  </h3>
                  <p className="mt-1.5 text-sm leading-relaxed text-gray-400">
                    {desc}
                  </p>
                </div>
              </div>
            ))}
          </div>
        </section>

        {/* Featured Collections */}
        <section className="mb-20">
          <div className="mb-8 flex items-center justify-between">
            <h2 className="font-heading text-2xl font-bold text-gray-100">
              Featured Collections
            </h2>
            <Link
              to="/projects"
              className="text-sm text-accent hover:underline"
            >
              View all &rarr;
            </Link>
          </div>
          <div className="grid gap-5 sm:grid-cols-3">
            {featured.map((c) => (
              <div
                key={c.id}
                className="group overflow-hidden rounded-xl border border-gray-800 bg-gray-900/50 transition-all hover:border-accent/40"
              >
                <div className="relative h-52 overflow-hidden">
                  <img
                    src={c.image}
                    alt={c.title}
                    className="h-full w-full object-cover transition-transform duration-500 group-hover:scale-105"
                    loading="lazy"
                  />
                  <div className="absolute inset-0 bg-gradient-to-t from-gray-950/80 to-transparent" />
                  <h3 className="absolute bottom-3 left-4 font-heading text-lg font-bold text-white">
                    {c.title}
                  </h3>
                  <span className="absolute right-3 top-3 rounded-full bg-accent/90 px-2 py-0.5 text-xs font-medium text-white">
                    {c.stars} sold
                  </span>
                </div>
                <div className="p-4">
                  <p className="mb-3 text-sm leading-relaxed text-gray-400">
                    {c.description}
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {c.tags.map((tag) => (
                      <span
                        key={tag}
                        className="rounded bg-accent/10 px-1.5 py-0.5 font-mono text-[10px] font-medium text-accent"
                      >
                        {tag}
                      </span>
                    ))}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>

        {/* Recent Journal */}
        <section className="mb-16">
          <div className="mb-8 flex items-center justify-between">
            <h2 className="font-heading text-2xl font-bold text-gray-100">
              From the Journal
            </h2>
            <Link
              to="/blog"
              className="text-sm text-accent hover:underline"
            >
              All entries &rarr;
            </Link>
          </div>
          <div className="grid gap-5 sm:grid-cols-2">
            {recentPosts.map((post) => (
              <PostCard key={post.id} post={post} />
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
