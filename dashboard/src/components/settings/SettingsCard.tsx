import type { LucideIcon } from 'lucide-react';

export default function SettingsCard({
  icon: Icon,
  title,
  description,
  children,
}: {
  icon: LucideIcon;
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-lg border border-slate-700/50 bg-slate-900/50 overflow-hidden">
      <header className="px-4 py-3 border-b border-slate-700/40 flex items-start gap-2.5">
        <Icon size={14} className="text-violet-400 mt-0.5 shrink-0" />
        <div>
          <h2 className="text-sm font-semibold text-slate-100">{title}</h2>
          {description && <p className="text-xs text-slate-500 mt-0.5">{description}</p>}
        </div>
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}
