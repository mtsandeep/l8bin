import { Loader2 } from 'lucide-react';
import type { GitHubBulkRunStatus } from '../../api';
import GithubMark from '../GithubMark';

interface Props {
  status: GitHubBulkRunStatus;
  onClick: () => void;
}

function dotFor(entry: GitHubBulkRunStatus): { bg: string; title: string } {
  if (entry.status === 'queued' || entry.status === 'in_progress' || entry.status === 'waiting') {
    return { bg: 'bg-violet-400', title: 'Deploy running' };
  }
  if (entry.status === 'completed') {
    if (entry.conclusion === 'success') return { bg: 'bg-emerald-400', title: 'Last deploy succeeded' };
    if (entry.conclusion === 'cancelled') return { bg: 'bg-slate-400', title: 'Last deploy cancelled' };
    return { bg: 'bg-red-400', title: `Last deploy ${entry.conclusion ?? 'failed'}` };
  }
  return { bg: 'bg-slate-500', title: 'No runs yet' };
}

export default function GitHubBadge({ status: entry, onClick }: Props) {
  const dot = dotFor(entry);
  const running = entry.status === 'queued' || entry.status === 'in_progress' || entry.status === 'waiting';

  return (
    <button
      type="button"
      onClick={onClick}
      title={`${entry.repo_full_name} — ${dot.title}`}
      className="relative text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
    >
      <GithubMark size={14} />
      {running ? (
        <Loader2 size={9} className="absolute -bottom-0.5 -right-1 animate-spin text-violet-400" />
      ) : (
        <span className={`absolute -bottom-0.5 -right-1 h-2 w-2 rounded-full ${dot.bg} ring-2 ring-slate-800`} />
      )}
    </button>
  );
}
