import { Loader2 } from 'lucide-react';
import GithubMark from '../GithubMark';
import type { GitHubBulkRunStatus } from '../../api';

interface Props {
  status: GitHubBulkRunStatus;
  onClick: () => void;
}

function dotFor(entry: GitHubBulkRunStatus): { bg: string; title: string } {
  if (entry.status === 'queued' || entry.status === 'in_progress' || entry.status === 'waiting') {
    return { bg: 'bg-violet-400', title: 'Deploy running' };
  }
  if (entry.status === 'completed') {
    return entry.conclusion === 'success'
      ? { bg: 'bg-emerald-400', title: 'Last deploy succeeded' }
      : { bg: 'bg-red-400', title: `Last deploy ${entry.conclusion ?? 'failed'}` };
  }
  return { bg: 'bg-slate-500', title: 'No runs yet' };
}

export default function GitHubBadge({ status: entry, onClick }: Props) {
  const repoShort = entry.repo_full_name.split('/')[1] ?? entry.repo_full_name;
  const dot = dotFor(entry);
  const running = entry.status === 'queued' || entry.status === 'in_progress' || entry.status === 'waiting';

  return (
    <button
      type="button"
      onClick={onClick}
      title={`${entry.repo_full_name} — ${dot.title}`}
      className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-slate-700/50 bg-slate-900/40 text-[10px] text-slate-400 hover:border-slate-600 hover:text-slate-300 transition-colors cursor-pointer"
    >
      <GithubMark size={10} />
      <span className="max-w-24 truncate">{repoShort}</span>
      {running ? (
        <Loader2 size={9} className="animate-spin text-violet-400" />
      ) : (
        <span className={`h-1.5 w-1.5 rounded-full ${dot.bg}`} />
      )}
    </button>
  );
}
