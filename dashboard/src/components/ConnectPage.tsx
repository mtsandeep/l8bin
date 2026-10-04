import { AlertTriangle, CheckCircle2, KeyRound, Loader2, ShieldAlert, XCircle } from 'lucide-react';
import { type FormEvent, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { approveDeviceRequest, type DeviceRequestInfo, fetchProjects, lookupDeviceRequest } from '../api';

const SCOPES = [
  { id: 'read', label: 'Read', description: 'View projects, status, and logs' },
  { id: 'deploy', label: 'Deploy', description: 'Read + build and deploy code' },
  { id: 'manage', label: 'Manage', description: 'Deploy + restart, settings, env' },
  { id: 'admin', label: 'Admin', description: 'Everything, including delete and nodes' },
] as const;

type ScopeId = (typeof SCOPES)[number]['id'];

type Phase =
  | { kind: 'entry' }
  | { kind: 'looking' }
  | { kind: 'review'; request: DeviceRequestInfo }
  | { kind: 'submitting'; request: DeviceRequestInfo }
  | { kind: 'done'; approved: boolean }
  | { kind: 'error'; message: string };

export default function ConnectPage() {
  const navigate = useNavigate();
  const [cells, setCells] = useState<string[]>(['', '', '', '', '', '']);
  const cellRefs = useRef<(HTMLInputElement | null)[]>([]);
  const [phase, setPhase] = useState<Phase>({ kind: 'entry' });
  const [scope, setScope] = useState<ScopeId>('deploy');
  const [projects, setProjects] = useState<{ id: string; name: string | null }[]>([]);
  const [projectId, setProjectId] = useState('');

  // Load projects once for the optional binding select
  useEffect(() => {
    fetchProjects()
      .then((ps) => setProjects(ps.map((p) => ({ id: p.id, name: p.name }))))
      .catch(() => setProjects([]));
  }, []);

  const userCode = `L8B-${cells.join('')}`;
  const codeComplete = cells.every((c) => c !== '');

  const setCell = (i: number, value: string) => {
    const ch = value
      .replace(/[^A-Za-z0-9]/g, '')
      .toUpperCase()
      .slice(-1);
    setCells((prev) => prev.map((c, k) => (k === i ? ch : c)));
    if (ch && i < 5) cellRefs.current[i + 1]?.focus();
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLInputElement>, i: number) => {
    e.preventDefault();
    const text = e.clipboardData.getData('text').toUpperCase();
    const stripped = text.replace(/^L8B-?/, '');
    const chars = stripped
      .replace(/[^A-Z0-9]/g, '')
      .slice(0, 6)
      .split('');
    if (chars.length === 0) return;
    setCells((prev) => {
      const next = [...prev];
      chars.forEach((c, k) => {
        if (i + k < 6) next[i + k] = c;
      });
      return next;
    });
    cellRefs.current[Math.min(i + chars.length, 5)]?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>, i: number) => {
    if (e.key === 'Backspace' && !cells[i] && i > 0) {
      e.preventDefault();
      setCells((prev) => prev.map((c, k) => (k === i - 1 ? '' : c)));
      cellRefs.current[i - 1]?.focus();
    }
  };

  const lookup = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!codeComplete) return;
    setPhase({ kind: 'looking' });
    try {
      const request = await lookupDeviceRequest(userCode);
      setScope(request.suggested_scope);
      setPhase({ kind: 'review', request });
    } catch (err) {
      setPhase({ kind: 'error', message: err instanceof Error ? err.message : 'Lookup failed' });
    }
  };

  const decide = async (approve: boolean) => {
    if (phase.kind !== 'review') return;
    setPhase({ kind: 'submitting', request: phase.request });
    try {
      await approveDeviceRequest({
        user_code: phase.request.user_code,
        approve,
        scope,
        project_id: projectId || undefined,
      });
      setPhase({ kind: 'done', approved: approve });
    } catch (err) {
      setPhase({ kind: 'error', message: err instanceof Error ? err.message : 'Approval failed' });
    }
  };

  return (
    <div className="min-h-screen bg-slate-950 flex items-center justify-center p-4">
      <div className="w-full max-w-md">
        <div className="flex items-center justify-center gap-3 mb-6">
          <KeyRound size={20} className="text-violet-400" />
          <h1 className="text-lg font-semibold text-slate-100">Connect a client</h1>
        </div>

        <div className="bg-slate-900/50 border border-slate-800/80 rounded-xl p-6 shadow-xl">
          {phase.kind === 'entry' || phase.kind === 'looking' || phase.kind === 'error' ? (
            <>
              <h2 className="text-sm font-medium text-slate-200 mb-1">Enter the pairing code</h2>
              <p className="text-xs text-slate-500 mb-5">
                The code was displayed by the client requesting access (e.g.{' '}
                <span className="font-mono">l8b login</span>
                ).
              </p>
              <form onSubmit={lookup} className="space-y-4">
                <div>
                  <div className="flex items-center justify-center gap-1.5">
                    <span className="text-sm font-mono font-semibold text-slate-500 tracking-widest">L8B-</span>
                    {cells.map((c, i) => (
                      <input
                        key={i}
                        ref={(el) => {
                          cellRefs.current[i] = el;
                        }}
                        type="text"
                        value={c}
                        onChange={(e) => {
                          setCell(i, e.target.value);
                          if (phase.kind === 'error') setPhase({ kind: 'entry' });
                        }}
                        onPaste={(e) => handlePaste(e, i)}
                        onKeyDown={(e) => handleKeyDown(e, i)}
                        maxLength={1}
                        disabled={phase.kind === 'looking'}
                        className="w-10 h-12 rounded-lg bg-slate-800/50 border border-slate-700/50 text-lg font-mono text-slate-200 text-center uppercase focus:outline-none focus:border-slate-400 focus:ring-1 focus:ring-slate-500/30 transition-colors"
                        aria-label={`Code character ${i + 1}`}
                      />
                    ))}
                  </div>
                  <p className="text-[11px] text-slate-600 text-center mt-2">
                    Paste the full code (L8B-…) into any cell to fill all six.
                  </p>
                </div>
                {phase.kind === 'error' && (
                  <div className="px-3 py-2.5 rounded-lg bg-rose-500/10 border border-rose-500/20">
                    <p className="text-xs text-rose-400">{phase.message}</p>
                    <p className="text-[11px] text-slate-500 mt-1">
                      Check the code on the client — codes expire after 10 minutes.
                    </p>
                  </div>
                )}
                <button
                  type="submit"
                  disabled={phase.kind === 'looking' || !codeComplete}
                  className="w-full flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 disabled:cursor-not-allowed transition-colors cursor-pointer"
                >
                  {phase.kind === 'looking' ? (
                    <>
                      <Loader2 size={14} className="animate-spin" /> Checking...
                    </>
                  ) : (
                    'Continue'
                  )}
                </button>
              </form>
              <div className="mt-5 flex items-start gap-2 px-3 py-2.5 rounded-lg bg-amber-500/5 border border-amber-500/20">
                <ShieldAlert size={14} className="text-amber-400 shrink-0 mt-0.5" />
                <p className="text-[11px] text-slate-400">
                  Only approve codes you initiated yourself. Approving grants the client a deploy token for this server.
                </p>
              </div>
            </>
          ) : phase.kind === 'review' || phase.kind === 'submitting' ? (
            <>
              <h2 className="text-sm font-medium text-slate-200 mb-1">Approve access</h2>
              <p className="text-xs text-slate-500 mb-4">
                <span className="font-mono text-slate-300">{phase.request.user_code}</span> is requesting access
                {phase.request.client_name ? (
                  <>
                    {' '}
                    from <span className="text-slate-300">{phase.request.client_name}</span>
                  </>
                ) : null}
                .
              </p>

              <div className="space-y-2 mb-4">
                <span className="block text-xs font-medium text-slate-400">Access level</span>
                {SCOPES.map((s) => (
                  <label
                    key={s.id}
                    className={`flex items-start gap-2.5 px-3 py-2 rounded-lg border cursor-pointer transition-colors ${
                      scope === s.id
                        ? 'bg-slate-600/30 border-slate-500'
                        : 'bg-slate-800/30 border-slate-700/50 hover:border-slate-600'
                    }`}
                  >
                    <input
                      type="radio"
                      name="scope"
                      checked={scope === s.id}
                      onChange={() => setScope(s.id)}
                      disabled={phase.kind === 'submitting'}
                      className="mt-0.5 accent-violet-500"
                    />
                    <span>
                      <span className="block text-xs font-medium text-slate-200">{s.label}</span>
                      <span className="block text-[11px] text-slate-500">{s.description}</span>
                    </span>
                  </label>
                ))}
              </div>

              <div className="mb-5">
                <label htmlFor="connect-project" className="block text-xs font-medium text-slate-400 mb-1.5">
                  Restrict to project (optional)
                </label>
                <select
                  id="connect-project"
                  value={projectId}
                  onChange={(e) => setProjectId(e.target.value)}
                  disabled={phase.kind === 'submitting'}
                  className="w-full px-3 py-2 rounded-lg bg-slate-800/50 border border-slate-700/50 text-sm text-slate-200 focus:outline-none focus:border-violet-500/50 transition-colors cursor-pointer"
                >
                  <option value="">All projects (global token)</option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name || p.id} ({p.id})
                    </option>
                  ))}
                </select>
                <p className="text-[11px] text-slate-500 mt-1">
                  A project-bound token can only access that project — recommended for agents working in one repo.
                </p>
              </div>

              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => decide(false)}
                  disabled={phase.kind === 'submitting'}
                  className="flex-1 px-4 py-2.5 rounded-lg text-sm font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  Deny
                </button>
                <button
                  type="button"
                  onClick={() => decide(true)}
                  disabled={phase.kind === 'submitting'}
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 transition-colors cursor-pointer"
                >
                  {phase.kind === 'submitting' ? <Loader2 size={14} className="animate-spin" /> : null}
                  Approve
                </button>
              </div>
            </>
          ) : (
            <div className="text-center py-4">
              {phase.approved ? (
                <>
                  <CheckCircle2 size={36} className="text-emerald-400 mx-auto mb-3" />
                  <p className="text-sm text-slate-200">Approved</p>
                  <p className="text-xs text-slate-500 mt-1">
                    The client will pick up its token automatically. You can revoke it anytime under{' '}
                    <span className="text-slate-400">Settings → Access Tokens</span>.
                  </p>
                </>
              ) : (
                <>
                  <XCircle size={36} className="text-slate-500 mx-auto mb-3" />
                  <p className="text-sm text-slate-200">Denied</p>
                  <p className="text-xs text-slate-500 mt-1">No token was issued.</p>
                </>
              )}
              <button
                type="button"
                onClick={() => navigate('/')}
                className="mt-5 px-4 py-2 rounded-lg text-xs font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 transition-colors cursor-pointer"
              >
                Back to dashboard
              </button>
            </div>
          )}
        </div>

        <p className="text-center mt-6 text-[11px] text-slate-600 flex items-center justify-center gap-1.5">
          <AlertTriangle size={11} />
          Approving a code you did not initiate gives someone else access to your server.
        </p>
      </div>
    </div>
  );
}
