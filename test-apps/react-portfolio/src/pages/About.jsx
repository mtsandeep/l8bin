import { profile, skills, experience } from "../data/seed";
import SkillBadge from "../components/SkillBadge";

const categories = [
  { key: "fabrics", label: "Fabrics" },
  { key: "techniques", label: "Techniques" },
  { key: "skills", label: "Expertise" },
  { key: "regions", label: "Regions" },
];

export default function About() {
  return (
    <div className="mx-auto max-w-5xl px-4 py-16">
      {/* Bio */}
      <section className="mb-16 flex flex-col gap-8 sm:flex-row sm:items-start">
        <div className="flex h-28 w-28 shrink-0 items-center justify-center rounded-2xl border border-gray-700 bg-accent/10 font-heading text-4xl font-bold text-accent">
          {profile.avatar}
        </div>
        <div>
          <h1 className="font-heading text-3xl font-bold text-gray-100">
            {profile.name}
          </h1>
          <p className="mt-1 text-lg text-accent">
            {profile.title}
          </p>
          <p className="mt-1 text-sm text-gray-500">
            {profile.location}
          </p>
          <p className="mt-4 max-w-2xl leading-relaxed text-gray-300">
            {profile.bio}
          </p>
        </div>
      </section>

      {/* Skills */}
      <section className="mb-16">
        <h2 className="mb-6 font-heading text-xl font-bold text-gray-100">
          What We Work With
        </h2>
        <div className="grid gap-6 sm:grid-cols-2">
          {categories.map(({ key, label }) => (
            <div key={key}>
              <h3 className="mb-3 text-sm font-semibold uppercase tracking-wider text-gray-400">
                {label}
              </h3>
              <div className="flex flex-wrap gap-2">
                {skills
                  .filter((s) => s.category === key)
                  .map((s) => (
                    <SkillBadge key={s.name} name={s.name} />
                  ))}
              </div>
            </div>
          ))}
        </div>
      </section>

      {/* Journey */}
      <section>
        <h2 className="mb-6 font-heading text-xl font-bold text-gray-100">
          The Journey
        </h2>
        <div className="space-y-6">
          {experience.map((job) => (
            <div
              key={job.year}
              className="rounded-xl border border-gray-800 bg-gray-900/50 p-5"
            >
              <div className="mb-1 flex items-center justify-between">
                <h3 className="font-heading font-semibold text-gray-100">
                  {job.role}
                </h3>
                <span className="font-mono text-xs text-gray-500">
                  {job.year}
                </span>
              </div>
              <p className="mb-2 text-sm text-accent">
                {job.company}
              </p>
              <p className="text-sm leading-relaxed text-gray-400">
                {job.description}
              </p>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
