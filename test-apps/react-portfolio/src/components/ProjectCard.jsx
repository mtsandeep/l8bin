import SkillBadge from "./SkillBadge";

export default function ProjectCard({ project }) {
  return (
    <div className="group rounded-xl border border-gray-800 bg-gray-900/50 p-5 transition-all hover:border-accent/40 hover:shadow-lg hover:shadow-accent/5">
      <div className="mb-3 flex items-start justify-between">
        <h3 className="font-heading text-lg font-semibold text-gray-100">
          {project.title}
        </h3>
        {project.featured && (
          <span className="rounded-full bg-accent/10 px-2 py-0.5 text-xs font-medium text-accent">
            Featured
          </span>
        )}
      </div>
      <p className="mb-4 text-sm leading-relaxed text-gray-400">
        {project.description}
      </p>
      <div className="flex items-center justify-between">
        <div className="flex flex-wrap gap-1.5">
          {project.tags.map((tag) => (
            <SkillBadge key={tag} name={tag} small />
          ))}
        </div>
        <span className="flex items-center gap-1 text-xs text-gray-500">
          <svg className="h-3.5 w-3.5" fill="currentColor" viewBox="0 0 24 24">
            <path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z" />
          </svg>
          {project.stars} sold
        </span>
      </div>
    </div>
  );
}
