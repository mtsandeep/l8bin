import { ExternalLink, KeyRound, Link2Off, Loader2, Rocket, X } from 'lucide-react';
import { useState } from 'react';
import {
  dispatchGithubDeploy,
  fetchGithubRuns,
  type GitHubRun,
  getProjectGithubConfig,
  patchProjectGithub,
  rotateGithubToken,
  unlinkProjectGithub,
} from '../../api';
import { useIntervalWhileVisible } from '../../hooks/useIntervalWhileVisible';
import GithubMark from '../GithubMark';
import { useToast } from '../ToastContext';

interface Props {
  projectId: string;
  onRefresh: () => void;
  onClose: () => void;
}

function runState(run: GitHubRun): { label: string; text: string; bg: string } {
  if (run.status === 'completed') {
    if (run.conclusion === 'success') return { label: 'success', text: 'text-emerald-400', bg: 'bg-emerald-400' };
    if (run.conclusion === 'cancelled') return { label: 'cancelled', text: 'text-slate-400', bg: 'bg-slate-400' };
    return { label: run.conclusion ?? 'failed', text: 'text-red-400', bg: 'bg-red-400' };
  }
  return { label: run.status ?? 'queued', text: 'text-violet-400', bg: 'bg-violet-400' };
}

export default function GitHubRunsModal({ projectId, onRefresh, onClose }: Props) {
  const [repo, setRepo] = useState<string | null>(null);
  const [runs, setRuns] = useState<GitHubRun[] | null>(null);
  const [runsError, setRunsError] = useState<string | null>(null);
  const [autoDeploy, setAutoDeploy] = useState<boolean | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [unlinkConfirm, setUnlinkConfirm] = useState(false);
  const { showToast } = useToast();

  const load = async () => {
    try {
      if (repo === null) {
        const cfg = await getProjectGithubConfig(projectId);
        setRepo(cfg.repo_full_name);
        setAutoDeploy(cfg.auto_deploy);
      }
      const res = await fetchGithubRuns(projectId);
      setRuns(res.runs);
      setRunsError(null);
    } catch (e) {
      setRunsError((e as Error).message);
      showToast((e as Error).message, 'error');
    }
  };

  useIntervalWhileVisible(load, 10000);

  const act = async (id: string, fn: () => Promise<unknown>, message: string) => {
    setBusy(id);
    try {
      await fn();
      showToast(message, 'success');
      onRefresh();
      await load();
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setBusy(null);
    }
  };

  const toggleAutoDeploy = () => {
    if (autoDeploy === null) return;
    const next = !autoDeploy;
    setAutoDeploy(next);
    act(
      'auto',
      () => patchProjectGithub(projectId, { auto_deploy: next }),
      next ? 'Auto-deploy enabled' : 'Auto-deploy disabled',
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50 shrink-0">
          <h2 className="text-sm font-semibold text-slate-100 flex items-center gap-2 truncate">
            <GithubMark size={14} className="shrink-0" />
            <span className="truncate">{repo ?? 'GitHub'}</span>
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        <div className="p-5 space-y-4 overflow-y-auto">
          <div className="flex items-center justify-between gap-2">
            <button
              type="button"
              onClick={() => act('deploy', () => dispatchGithubDeploy(projectId), 'Deploy dispatched')}
              disabled={busy !== null}
              className="flex-1 py-2 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 text-xs font-medium text-white transition-colors cursor-pointer flex items-center justify-center gap-1.5"
            >
              {busy === 'deploy' ? <Loader2 size={12} className="animate-spin" /> : <Rocket size={12} />}
              Deploy now
            </button>
            <div className="flex items-center gap-2 px-1">
              <span className="text-xs text-slate-400">Auto on push</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoDeploy ?? false}
                onClick={toggleAutoDeploy}
                disabled={busy !== null || autoDeploy === null}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  autoDeploy ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    autoDeploy ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </div>
          </div>

          <div className="space-y-1.5">
            {runs === null && !runsError && (
              <p className="text-xs text-slate-500 text-center py-4 flex items-center justify-center gap-1.5">
                <Loader2 size={11} className="animate-spin" />
                Loading deployments…
              </p>
            )}
            {runsError && <p className="text-xs text-amber-300/90 text-center py-4">{runsError}</p>}
            {runs !== null && !runsError && runs.length === 0 && (
              <p className="text-xs text-slate-500 text-center py-4">No runs yet.</p>
            )}
            {runs?.slice(0, 10).map((run) => {
              const state = runState(run);
              return (
                <a
                  key={run.id}
                  href={run.html_url}
                  target="_blank"
                  rel="noreferrer"
                  className="flex items-center gap-2 rounded-md border border-slate-700/40 bg-slate-900/40 px-3 py-2 hover:border-slate-600 transition-colors"
                >
                  {run.status !== 'completed' ? (
                    <Loader2 size={12} className="animate-spin text-violet-400 shrink-0" />
                  ) : (
                    <span className={`h-1.5 w-1.5 rounded-full shrink-0 ${state.bg}`} />
                  )}
                  <span className="text-xs text-slate-300">#{run.run_number}</span>
                  <span className={`text-xs ${state.text}`}>{state.label}</span>
                  <span className="text-xs text-slate-500 truncate">
                    {run.head_branch ? `@${run.head_branch}` : ''}
                    {run.head_sha ? ` ${run.head_sha.slice(0, 7)}` : ''}
                  </span>
                  <ExternalLink size={11} className="ml-auto text-slate-500 shrink-0" />
                </a>
              );
            })}
          </div>

          <div className="flex items-center gap-3 pt-2 border-t border-slate-700/40">
            <button
              type="button"
              onClick={() => act('rotate', () => rotateGithubToken(projectId), 'Deploy token rotated')}
              disabled={busy !== null}
              className="text-xs text-slate-400 hover:text-slate-200 flex items-center gap-1 transition-colors cursor-pointer disabled:opacity-50"
            >
              {busy === 'rotate' ? <Loader2 size={11} className="animate-spin" /> : <KeyRound size={11} />}
              Rotate token
            </button>
            {unlinkConfirm ? (
              <div className="ml-auto flex items-center gap-2">
                <span className="text-xs text-slate-400">Remove workflow + secrets?</span>
                <button
                  type="button"
                  onClick={() =>
                    act('unlink', () => unlinkProjectGithub(projectId, true), 'GitHub link removed').then(() =>
                      onClose(),
                    )
                  }
                  disabled={busy !== null}
                  className="text-xs text-red-400 hover:text-red-300 transition-colors cursor-pointer disabled:opacity-50"
                >
                  Confirm
                </button>
                <button
                  type="button"
                  onClick={() => setUnlinkConfirm(false)}
                  className="text-xs text-slate-500 hover:text-slate-400 transition-colors cursor-pointer"
                >
                  Cancel
                </button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setUnlinkConfirm(true)}
                disabled={busy !== null}
                className="ml-auto text-xs text-slate-400 hover:text-red-300 flex items-center gap-1 transition-colors cursor-pointer disabled:opacity-50"
              >
                <Link2Off size={11} />
                Unlink repo
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
