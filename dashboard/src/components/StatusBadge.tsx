interface StatusBadgeProps {
  status: string;
}

const statusConfig: Record<string, { bg: string; text: string; dot: string; label: string }> = {
  running: {
    bg: 'bg-emerald-500/10',
    text: 'text-emerald-400',
    dot: 'bg-emerald-400',
    label: 'Running',
  },
  stopped: {
    bg: 'bg-slate-500/10',
    text: 'text-slate-400',
    dot: 'bg-slate-400',
    label: 'Stopped',
  },
  deploying: {
    bg: 'bg-amber-500/10',
    text: 'text-amber-400',
    dot: 'bg-amber-400 animate-pulse',
    label: 'Deploying',
  },
  stopping: {
    bg: 'bg-orange-500/10',
    text: 'text-orange-400',
    dot: 'bg-orange-400 animate-pulse',
    label: 'Stopping',
  },
  waking: {
    bg: 'bg-sky-500/10',
    text: 'text-sky-400',
    dot: 'bg-sky-400 animate-pulse',
    label: 'Waking',
  },
  unconfigured: {
    bg: 'bg-indigo-500/10',
    text: 'text-indigo-400',
    dot: 'bg-indigo-400',
    label: 'Pending',
  },
};

export default function StatusBadge({ status }: StatusBadgeProps) {
  const config = statusConfig[status] ?? {
    bg: 'bg-slate-500/10',
    text: 'text-slate-400',
    dot: 'bg-slate-400',
    label: status,
  };

  return (
    <span
      className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs font-medium ${config.bg} ${config.text}`}
    >
      <span className={`w-1.5 h-1.5 rounded-full ${config.dot}`} />
      {config.label}
    </span>
  );
}
