import { AlertTriangle, EyeOff, FileKey2, Loader2, RefreshCw, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { fetchProjectEnv, type ProjectEnv, recreateProject, startProject, updateProjectEnv } from '../../api';
import { useToast } from '../ToastContext';

interface EnvModalProps {
  projectId: string;
  status: string;
  onRefresh: () => void;
  onClose: () => void;
}

const UNCONFIGURED = 'unconfigured';

export default function EnvModal({ projectId, status, onRefresh, onClose }: EnvModalProps) {
  const { showToast } = useToast();
  const [env, setEnv] = useState<ProjectEnv | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [replace, setReplace] = useState(false);
  const [saving, setSaving] = useState(false);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      setEnv(await fetchProjectEnv(projectId));
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load env');
    } finally {
      setLoading(false);
    }
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: reload only when the target project changes
  useEffect(() => {
    load();
  }, [projectId]);

  const startEditing = () => {
    setEditing(true);
    setDraft('');
    setReplace(false);
  };

  const save = async (apply: boolean) => {
    const vars: Record<string, string> = {};
    let ok = true;
    for (const line of draft.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith('#')) continue;
      const eq = trimmed.indexOf('=');
      if (eq <= 0) {
        setError(`Invalid line (expected KEY=VALUE): ${trimmed}`);
        ok = false;
        break;
      }
      vars[trimmed.slice(0, eq).trim()] = trimmed
        .slice(eq + 1)
        .trim()
        .replace(/^["']|["']$/g, '');
    }
    if (!ok || Object.keys(vars).length === 0) {
      if (ok) setError('Nothing to save — add at least one KEY=VALUE line.');
      return;
    }

    setSaving(true);
    setError(null);
    try {
      const updated = await updateProjectEnv(projectId, { env: vars, mode: replace ? 'replace' : 'merge' });
      if (apply) {
        if (status === UNCONFIGURED) {
          await startProject(projectId);
        } else {
          const warnings = await recreateProject(projectId);
          for (const w of warnings) showToast(w, 'warning');
        }
        showToast('Environment saved and applied', 'success');
        onRefresh();
      } else {
        showToast('Environment saved — applies on next start', 'success');
      }
      setEnv(updated);
      setEditing(false);
      setDraft('');
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to save env');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-slate-950/80 backdrop-blur-sm">
      <div className="w-full max-w-md bg-slate-900/90 border border-slate-800 rounded-xl shadow-xl max-h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-800">
          <div className="flex items-center gap-2">
            <FileKey2 size={16} className="text-violet-400" />
            <h2 className="text-sm font-medium text-slate-200">Environment — {projectId}</h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-1.5 rounded-lg text-slate-500 hover:text-slate-300 hover:bg-slate-800 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="p-5 space-y-4 overflow-y-auto">
          {error && (
            <div className="px-3 py-2.5 rounded-lg bg-rose-500/10 border border-rose-500/20">
              <p className="text-xs text-rose-400">{error}</p>
            </div>
          )}

          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 size={20} className="animate-spin text-slate-500" />
            </div>
          ) : editing ? (
            <>
              <div className="flex items-start gap-2 px-3 py-2.5 rounded-lg bg-slate-800/40 border border-slate-700/50">
                <EyeOff size={14} className="text-slate-500 shrink-0 mt-0.5" />
                <p className="text-[11px] text-slate-400">
                  Values are write-only — existing values are never shown. Enter the variables to set, one KEY=VALUE per
                  line.
                  {replace ? ' Replace mode removes any key not listed.' : ' Keys not listed are kept.'}
                </p>
              </div>
              <textarea
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder={'DATABASE_URL=postgres://...\nSESSION_SECRET=...'}
                rows={7}
                spellCheck={false}
                className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-xs text-slate-200 placeholder-slate-600 font-mono focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                disabled={saving}
              />
              <label className="flex items-center gap-2 text-xs text-slate-400 cursor-pointer">
                <input
                  type="checkbox"
                  checked={replace}
                  onChange={(e) => setReplace(e.target.checked)}
                  disabled={saving}
                  className="accent-violet-500"
                />
                Replace all (remove keys not listed)
              </label>
              <div className="flex gap-2 pt-1">
                <button
                  type="button"
                  onClick={() => {
                    setEditing(false);
                    setError(null);
                  }}
                  disabled={saving}
                  className="flex-1 px-4 py-2 rounded-lg text-xs font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => save(false)}
                  disabled={saving}
                  className="flex-1 px-4 py-2 rounded-lg text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  Save
                </button>
                <button
                  type="button"
                  onClick={() => save(true)}
                  disabled={saving}
                  className="flex-1 flex items-center justify-center gap-1.5 px-4 py-2 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  {saving ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
                  Save{status === UNCONFIGURED ? ' & start' : ' & apply'}
                </button>
              </div>
            </>
          ) : env ? (
            <>
              {env.vars.length === 0 ? (
                <p className="text-xs text-slate-500 py-3 text-center">
                  No environment variables set. Add runtime secrets (database URLs, API keys) before starting.
                </p>
              ) : (
                <div className="space-y-1.5">
                  {env.vars.map((v) => (
                    <div
                      key={v.key}
                      className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg bg-slate-800/40 border border-slate-700/40"
                    >
                      <span className="text-xs font-mono text-slate-300 truncate">{v.key}</span>
                      <span className="text-xs font-mono text-amber-400/80 shrink-0" title="Value is write-only">
                        {v.masked || '(empty)'}
                        <span className="text-slate-600 ml-1.5">{v.length} chars</span>
                      </span>
                    </div>
                  ))}
                </div>
              )}

              {env.pending_apply && (
                <div className="flex items-start gap-2 px-3 py-2.5 rounded-lg bg-amber-500/5 border border-amber-500/20">
                  <AlertTriangle size={14} className="text-amber-400 shrink-0 mt-0.5" />
                  <p className="text-[11px] text-slate-400">
                    Pending: these changes are not in the running container yet — restart to apply.
                  </p>
                </div>
              )}

              <button
                type="button"
                onClick={startEditing}
                className="w-full px-4 py-2 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer"
              >
                {env.vars.length === 0 ? 'Add variables' : 'Update variables'}
              </button>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}
