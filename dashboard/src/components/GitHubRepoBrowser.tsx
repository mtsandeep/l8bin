import { Loader2, Lock, Search, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { type GitHubConnectionSummary, type GitHubRepo, listGithubConnections, listGithubRepos } from '../api';
import GitHubDeployForm from './GitHubDeployForm';
import GithubMark from './GithubMark';

interface Props {
  domain: string;
  /** Link mode: bind the picked repo to this existing project. */
  fixedProjectId?: string;
  onClose: () => void;
  onLinked?: () => void;
}

function timeAgoStr(iso: string | null): string {
  if (!iso) return '';
  const secs = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (secs < 3600) return `${Math.max(1, Math.floor(secs / 60))}m ago`;
  if (secs < 86400 * 30) return `${Math.floor(secs / 86400)}d ago`;
  return new Date(iso).toLocaleDateString();
}

export default function GitHubRepoBrowser({ domain, fixedProjectId, onClose, onLinked }: Props) {
  const [repos, setRepos] = useState<GitHubRepo[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<GitHubRepo | null>(null);
  type Pick = { installationId?: number; connectionId?: number };
  const [connections, setConnections] = useState<GitHubConnectionSummary[] | null | undefined>(undefined);
  const [pick, setPick] = useState<Pick>({});

  const entries: Array<Pick & { key: string; label: string }> = [];
  for (const conn of connections ?? []) {
    if (conn.mode === 'app') {
      for (const inst of conn.installations) {
        entries.push({ installationId: inst.id, key: `i-${inst.id}`, label: inst.account_login });
      }
    } else {
      entries.push({ connectionId: conn.id, key: `c-${conn.id}`, label: `${conn.label ?? 'token'} (token)` });
    }
  }

  useEffect(() => {
    listGithubConnections()
      .then(setConnections)
      .catch(() => setConnections(null));
  }, []);

  const load = async (targetPage: number, replace: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const res = await listGithubRepos(targetPage, pick.installationId, pick.connectionId);
      setRepos((prev) => (replace ? res.repos : [...prev, ...res.repos]));
      setHasMore(res.repos.length === 100);
      setPage(targetPage);
    } catch (e) {
      setError((e as Error).message);
      if (replace) setRepos([]);
    } finally {
      setLoading(false);
    }
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload on mode/account change
  useEffect(() => {
    if (connections === undefined) return;
    if (!pick.installationId && !pick.connectionId) {
      if (entries.length > 0) {
        const first = entries[0];
        setPick({ installationId: first.installationId, connectionId: first.connectionId });
      }
      return;
    }
    load(1, true);
    // biome-ignore lint/correctness/useExhaustiveDependencies: reload on mode/account change
  }, [connections, pick]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return repos;
    return repos.filter(
      (r) => r.full_name.toLowerCase().includes(q) || (r.description ?? '').toLowerCase().includes(q),
    );
  }, [repos, query]);

  if (selected) {
    return (
      <GitHubDeployForm
        repo={selected}
        domain={domain}
        installationId={pick.installationId}
        connectionId={pick.connectionId}
        fixedProjectId={fixedProjectId}
        onBack={() => setSelected(null)}
        onClose={onClose}
        onLinked={() => {
          onLinked?.();
          onClose();
        }}
      />
    );
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-lg mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50 shrink-0">
          <h2 className="text-sm font-semibold text-slate-100 flex items-center gap-2">
            <GithubMark size={14} />
            Deploy from GitHub
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-3 border-b border-slate-700/50 shrink-0">
          <div className="relative">
            <Search size={13} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-500" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search repositories…"
              className="w-full pl-8 pr-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
            />
          </div>
        </div>

        {entries.length > 1 && (
          <div className="px-5 py-2 border-b border-slate-700/50 shrink-0 flex items-center gap-2">
            <span className="text-xs text-slate-500 uppercase">Account</span>
            <select
              value={
                pick.installationId ? `i-${pick.installationId}` : pick.connectionId ? `c-${pick.connectionId}` : ''
              }
              onChange={(e) => {
                const v = e.target.value;
                if (v.startsWith('i-')) setPick({ installationId: Number(v.slice(2)) });
                else if (v.startsWith('c-')) setPick({ connectionId: Number(v.slice(2)) });
              }}
              className="flex-1 px-2 py-1 bg-slate-900/50 border border-slate-700/50 rounded text-xs text-slate-200 focus:outline-none focus:border-violet-500/50 cursor-pointer"
            >
              {entries.map((e) => (
                <option key={e.key} value={e.key}>
                  {e.label}
                </option>
              ))}
            </select>
          </div>
        )}

        <div className="overflow-y-auto flex-1">
          {error && (
            <div className="px-5 py-8 text-center">
              <p className="text-xs text-red-400">{error}</p>
              <button
                type="button"
                onClick={() => load(1, true)}
                className="mt-3 text-xs text-violet-400 hover:text-violet-300 cursor-pointer"
              >
                Retry
              </button>
            </div>
          )}

          {!error && loading && repos.length === 0 && (
            <div className="px-5 py-8 flex justify-center">
              <Loader2 size={16} className="animate-spin text-slate-500" />
            </div>
          )}

          {filtered.map((repo) => (
            <button
              key={repo.id}
              type="button"
              onClick={() => setSelected(repo)}
              className="w-full text-left px-5 py-3 hover:bg-slate-700/30 transition-colors cursor-pointer border-b border-slate-700/30"
            >
              <div className="flex items-center gap-2">
                {repo.private && <Lock size={11} className="text-slate-500 shrink-0" />}
                <span className="text-xs font-medium text-slate-200 truncate">{repo.full_name}</span>
                <span className="ml-auto text-xs text-slate-500 shrink-0">{timeAgoStr(repo.pushed_at)}</span>
              </div>
              {repo.description && <p className="text-xs text-slate-500 mt-0.5 truncate">{repo.description}</p>}
            </button>
          ))}

          {!error && !loading && filtered.length === 0 && (
            <p className="px-5 py-8 text-center text-xs text-slate-500">
              {query ? 'No repositories match your search.' : 'No repositories found.'}
            </p>
          )}
        </div>

        {!query && hasMore && !error && (
          <div className="px-5 py-3 border-t border-slate-700/50 shrink-0">
            <button
              type="button"
              onClick={() => load(page + 1, false)}
              disabled={loading}
              className="w-full py-2 rounded-md border border-slate-700/50 hover:border-slate-600 text-xs text-slate-300 transition-colors cursor-pointer disabled:opacity-50 flex items-center justify-center gap-1.5"
            >
              {loading && <Loader2 size={12} className="animate-spin" />}
              Load more
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
