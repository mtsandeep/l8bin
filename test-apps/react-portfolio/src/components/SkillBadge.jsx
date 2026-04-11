export default function SkillBadge({ name, small }) {
  return (
    <span
      className={`inline-block rounded-md bg-accent/10 font-mono font-medium text-accent ${
        small
          ? "px-1.5 py-0.5 text-[10px]"
          : "px-2.5 py-1 text-xs"
      }`}
    >
      {name}
    </span>
  );
}
