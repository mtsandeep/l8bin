import { EyeOff, FileKey2, Loader2, Plus, RefreshCw, Trash2, X } from 'lucide-react';
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

const inputClass =
  'w-full px-2.5 py-1.5 rounded-lg bg-slate-800/50 border border-slate-700/50 text-xs text-slate-200 placeholder-slate-600 font-mono focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors';

export default function EnvModal({ projectId, status, onRefresh, onClose }: EnvModalProps) {
  const { showToast } = useToast();
  const [env, setEnv] = useState<ProjectEnv | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);

  const [editKey, setEditKey] = useState<string | null>(null);
  const [editValue, setEditValue] = useState('');
  const [newKey, setNewKey] = useState('');
  const [newValue, setNewValue] = useState('');
  const [view, setView] = useState<'list' | 'bulk'>('list');
  const [bulkText, setBulkText] = useState('');
  const [bulkReplace, setBulkReplace] = useState(false);

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

  const push = async (
    payload: { env?: Record<string, string>; delete?: string[]; mode?: 'merge' | 'replace' },
    key: string | null,
    apply: boolean,
    successMsg: string,
  ) => {
    setBusyKey(key);
    setError(null);
    try {
      const body: { env: Record<string, string>; delete?: string[]; mode?: 'merge' | 'replace' } = {
        env: payload.env ?? {},
      };
      if (payload.delete && payload.delete.length > 0) body.delete = payload.delete;
      if (payload.mode) body.mode = payload.mode;
      const updated = await updateProjectEnv(projectId, body);
      setEnv(updated);
      if (apply) {
        setApplying(true);
        if (status === UNCONFIGURED) {
          await startProject(projectId);
        } else {
          const warnings = await recreateProject(projectId);
          for (const w of warnings) showToast(w, 'warning');
        }
        showToast(successMsg, 'success');
        onRefresh();
        setApplying(false);
      } else {
        showToast(successMsg, 'success');
      }
      return true;
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to update env');
      return false;
    } finally {
      setBusyKey(null);
    }
  };

  const validKey = (k: string) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(k);

  const saveEdit = async (key: string, apply: boolean) => {
    if (editValue === '') {
      setError('Enter a value (or cancel).');
      return;
    }
    const ok = await push({ env: { [key]: editValue } }, key, apply, `Updated ${key}`);
    if (ok) {
      setEditKey(null);
      setEditValue('');
    }
  };

  const addVar = async () => {
    if (!validKey(newKey)) {
      setError(`Invalid key '${newKey}': must match [A-Za-z_][A-Za-z0-9_]*`);
      return;
    }
    if (env?.vars.some((v) => v.key === newKey)) {
      setError(`${newKey} already exists — use its update button instead`);
      return;
    }
    const ok = await push({ env: { [newKey]: newValue } }, newKey, false, `Added ${newKey}`);
    if (ok) {
      setNewKey('');
      setNewValue('');
    }
  };

  const removeVar = async (key: string) => {
    const ok = await push({ env: {}, delete: [key] }, key, false, `Removed ${key}`);
    if (ok && editKey === key) {
      setEditKey(null);
      setEditValue('');
    }
  };

  const saveBulk = async (apply: boolean) => {
    const vars: Record<string, string> = {};
    for (const line of bulkText.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith('#')) continue;
      const eq = trimmed.indexOf('=');
      if (eq <= 0) {
        setError(`Invalid line (expected KEY=VALUE): ${trimmed}`);
        return;
      }
      vars[trimmed.slice(0, eq).trim()] = trimmed
        .slice(eq + 1)
        .trim()
        .replace(/^["']|["']$/g, '');
    }
    if (Object.keys(vars).length === 0) {
      setError('Nothing to save — add at least one KEY=VALUE line.');
      return;
    }
    const ok = await push({ env: vars, mode: bulkReplace ? 'replace' : 'merge' }, null, apply, 'Environment updated');
    if (ok) {
      setView('list');
      setBulkText('');
      setBulkReplace(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-slate-950/80 backdrop-blur-sm">
      <div className="w-full max-w-lg bg-slate-900/90 border border-slate-800 rounded-xl shadow-xl max-h-[85vh] flex flex-col">
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
          ) : view === 'bulk' ? (
            (
              <div className="space-y-3">
              <div className="flex items-start gap-2 px-3 py-2.5 rounded-lg bg-slate-800/40 border border-slate-700/50">
                <EyeOff size={14} className="text-slate-500 shrink-0 mt-0.5" />
                <p className="text-[11px] text-slate-400">
                  Paste a full environment below, one KEY=VALUE per line. Existing values are never shown.
                </p>
              </div>
              <textarea
                value={bulkText}
                onChange={(e) => setBulkText(e.target.value)}
                placeholder={'DATABASE_URL=postgres://...\nSESSION_SECRET=...\nAPI_KEY=...'}
                rows={10}
                spellCheck={false}
                className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-xs text-slate-200 placeholder-slate-600 font-mono focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/20 transition-colors"
                disabled={busyKey !== null}
              />
              <label className="flex items-center gap-2 text-xs text-slate-400 cursor-pointer">
                <input
                  type="checkbox"
                  checked={bulkReplace}
                  onChange={(e) => setBulkReplace(e.target.checked)}
                  disabled={busyKey !== null}
                  className="accent-violet-500"
                />
                Replace all (remove keys not present in the paste)
              </label>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => {
                    setView('list');
                    setBulkText('');
                    setBulkReplace(false);
                    setError(null);
                  }}
                  disabled={busyKey !== null}
                  className="flex-1 px-3 py-2 rounded-lg text-xs font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  Back
                </button>
                <button
                  type="button"
                  onClick={() => saveBulk(false)}
                  disabled={busyKey !== null}
                  className="flex-1 px-3 py-2 rounded-lg text-xs font-medium bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  Save{bulkReplace ? ' (replace)' : ' (merge)'}
                </button>
                <button
                  type="button"
                  onClick={() => saveBulk(true)}
                  disabled={busyKey !== null}
                  className="flex-1 flex items-center justify-center gap-1.5 px-3 py-2 rounded-lg text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  {applying ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
                  Save{status === UNCONFIGURED ? ' & start' : ' & apply'}
                </button>
              </div>
            </div>
            )
          ) : (
            <>
              <div className="flex items-start gap-2 px-3 py-2.5 rounded-lg bg-slate-800/40 border border-slate-700/50">
                <EyeOff size={14} className="text-slate-500 shrink-0 mt-0.5" />
                <p className="text-[11px] text-slate-400">
                  Values are write-only — they are never displayed. Update a key to replace its value.
                </p>
              </div>
              {/* Per-key rows */}
              {env && env.vars.length > 0 && (
                <div className="space-y-1.5">
                  {env.vars.map((v) => (
                    <div
                      key={v.key}
                      className="px-3 py-2 rounded-lg bg-slate-800/40 border border-slate-700/40 space-y-2"
                    >
                      <div className="flex items-center justify-between gap-3">
                        <span className="text-xs font-mono text-slate-300 truncate">{v.key}</span>
                        {editKey === v.key ? null : (
                          <span className="flex items-center gap-1 shrink-0">
                            <span className="text-[10px] font-mono text-amber-400/70" title="Value is write-only">
                              {v.masked || '(empty)'}
                            </span>
                            <button
                              type="button"
                              onClick={() => {
                                setEditKey(v.key);
                                setEditValue('');
                                setError(null);
                              }}
                              disabled={busyKey !== null}
                              className="text-[10px] px-1.5 py-0.5 rounded text-violet-300 hover:bg-violet-500/10 transition-colors cursor-pointer disabled:opacity-50"
                            >
                              Update
                            </button>
                            <button
                              type="button"
                              onClick={() => removeVar(v.key)}
                              disabled={busyKey !== null}
                              className="p-1 rounded text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50"
                              title={`Remove ${v.key}`}
                            >
                              {busyKey === v.key ? (
                                <Loader2 size={11} className="animate-spin" />
                              ) : (
                                <Trash2 size={11} />
                              )}
                            </button>
                          </span>
                        )}
                      </div>
                      {editKey === v.key && (
                        <div className="space-y-2">
                          <input
                            type="password"
                            value={editValue}
                            onChange={(e) => setEditValue(e.target.value)}
                            placeholder={`New value for ${v.key}`}
                            className={inputClass}
                            disabled={busyKey !== null}
                          />
                          <div className="flex gap-1.5">
                            <button
                              type="button"
                              onClick={() => {
                                setEditKey(null);
                                setEditValue('');
                                setError(null);
                              }}
                              disabled={busyKey !== null}
                              className="flex-1 px-2 py-1 rounded text-[11px] bg-slate-800 text-slate-300 hover:bg-slate-700 transition-colors cursor-pointer disabled:opacity-50"
                            >
                              Cancel
                            </button>
                            <button
                              type="button"
                              onClick={() => saveEdit(v.key, false)}
                              disabled={busyKey !== null || editValue === ''}
                              className="flex-1 px-2 py-1 rounded text-[11px] bg-slate-700 text-slate-200 hover:bg-slate-600 transition-colors cursor-pointer disabled:opacity-50"
                            >
                              Save
                            </button>
                            <button
                              type="button"
                              onClick={() => saveEdit(v.key, true)}
                              disabled={busyKey !== null || editValue === ''}
                              className="flex-1 flex items-center justify-center gap-1 px-2 py-1 rounded text-[11px] bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer disabled:opacity-50"
                            >
                              {busyKey === v.key ? (
                                <Loader2 size={10} className="animate-spin" />
                              ) : (
                                <RefreshCw size={10} />
                              )}
                              Save{status === UNCONFIGURED ? ' & start' : ' & apply'}
                            </button>
                          </div>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
              {env?.pending_apply && (
                <div className="flex items-start gap-2 px-3 py-2.5 rounded-lg bg-amber-500/5 border border-amber-500/20">
                  <span className="text-amber-400 text-xs">!</span>
                  <p className="text-[11px] text-slate-400">
                    Pending: saved changes are not in the running container yet — restart to apply.
                  </p>
                </div>
              )}
              {/* Add a key */}
              <div className="border-t border-slate-700/50 pt-3 space-y-2">
                <span className="block text-xs font-medium text-slate-400 flex items-center gap-1.5">
                  <Plus size={11} /> Add variable
                </span>
                <input
                  value={newKey}
                  onChange={(e) => {
                    setNewKey(e.target.value);
                    setError(null);
                  }}
                  placeholder="KEY"
                  className={inputClass}
                  disabled={busyKey !== null}
                />
                <div className="flex gap-1.5">
                  <input
                    type="password"
                    value={newValue}
                    onChange={(e) => setNewValue(e.target.value)}
                    placeholder="value"
                    className={`${inputClass} flex-1`}
                    disabled={busyKey !== null}
                  />
                  <button
                    type="button"
                    onClick={addVar}
                    disabled={busyKey !== null || !newKey || newValue === ''}
                    className="px-3 rounded text-xs bg-violet-600 text-white hover:bg-violet-500 transition-colors cursor-pointer disabled:opacity-50"
                  >
                    Add
                  </button>
                </div>
              </div>
              {/* Bulk paste entry */}
              <div className="border-t border-slate-700/50 pt-3">
                <button
                  type="button"
                  onClick={() => {
                    setView('bulk');
                    setError(null);
                  }}
                  className="text-[11px] text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
                >
                  + Bulk paste a full .env file
                </button>
              </div>{' '}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
