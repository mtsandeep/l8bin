import {
  AlertTriangle,
  ArrowRight,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  Copy,
  ExternalLink,
  FileKey2,
  KeyRound,
  Loader2,
  Lock,
  Plus,
  Workflow,
  X,
} from 'lucide-react';
import { type ReactNode, useEffect, useState } from 'react';
import {
  connectGithubApp,
  connectGithubPat,
  disconnectGithub,
  fetchGithubConnection,
  type GitHubConnection,
  type GitHubConnectionSummary,
  type GitHubRepo,
  listGithubConnections,
  listGithubInstallations,
  listGithubRepos,
  startGithubManifest,
} from '../../api';
import { useIntervalWhileVisible } from '../../hooks/useIntervalWhileVisible';
import GithubMark from '../GithubMark';
import { useToast } from '../ToastContext';

type View = 'choose' | 'guided' | 'manual-setup' | 'manual-attach' | 'pat';

const inputClass =
  'w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors';

const buttonBase =
  'flex items-center justify-center gap-1.5 py-2.5 px-4 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 disabled:hover:bg-violet-600 text-xs font-medium text-white transition-colors cursor-pointer';

const buttonPrimary = `${buttonBase} w-full`;

function Segments({ items, current = 0 }: { items: string[]; current?: number }) {
  return (
    <div className="flex w-full">
      {items.map((label, i) => {
        const done = i < current;
        const active = i === current;
        const clip =
          i === 0
            ? 'polygon(0 0, calc(100% - 10px) 0, 100% 50%, calc(100% - 10px) 100%, 0 100%)'
            : 'polygon(0 0, calc(100% - 10px) 0, 100% 50%, calc(100% - 10px) 100%, 0 100%, 10px 50%)';
        return (
          <div
            key={label}
            title={label}
            className={`flex-1 flex items-center justify-center gap-1.5 h-9 px-4 min-w-0 text-xs font-medium
              ${i > 0 ? 'ml-0.5' : ''}
              ${
                done
                  ? 'bg-violet-500/30 text-violet-200'
                  : active
                    ? 'bg-violet-600/80 text-white'
                    : 'bg-slate-800 text-slate-500'
              }`}
            style={{ clipPath: clip }}
          >
            <span className="shrink-0 leading-none">{done ? <Check size={13} /> : i + 1}</span>
            <span className="truncate">{label}</span>
          </div>
        );
      })}
    </div>
  );
}

function Disclosure({
  summary,
  children,
  open: openProp,
  onToggle,
}: {
  summary: string;
  children: ReactNode;
  open?: boolean;
  onToggle?: () => void;
}) {
  const [openState, setOpenState] = useState(false);
  const open = openProp ?? openState;
  const toggle = () => (onToggle ? onToggle() : setOpenState((v) => !v));
  return (
    <div>
      <button
        type="button"
        onClick={toggle}
        className="flex items-center gap-1 text-xs text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
      >
        <ChevronDown size={11} className={`transition-transform ${open ? 'rotate-180' : ''}`} />
        {summary}
      </button>
      {open && <div className="mt-2.5 space-y-2">{children}</div>}
    </div>
  );
}

function CopyRow({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex items-center gap-2 py-0.5">
      <span className="text-xs text-slate-400 shrink-0 w-20">{label}</span>
      <span className="text-xs font-mono text-slate-300 truncate flex-1" title={value}>
        {value}
      </span>
      <button
        type="button"
        aria-label={`Copy ${label}`}
        onClick={() => {
          navigator.clipboard.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
        className="p-1 rounded text-slate-500 hover:text-slate-300 transition-colors cursor-pointer shrink-0"
      >
        {copied ? <CheckCircle2 size={12} className="text-emerald-400" /> : <Copy size={12} />}
      </button>
    </div>
  );
}

const PERMISSIONS: Array<[string, string, string]> = [
  ['Contents', 'Read & write', 'commits the deploy workflow to your repo'],
  ['Workflows', 'Read & write', 'authors files under .github/workflows/'],
  ['Actions', 'Read & write', 'triggers deploys, reads run status'],
  ['Secrets', 'Read & write', 'stores the deploy token + server URL'],
  ['Metadata', 'Read-only', 'lists your repositories'],
];

function PermissionsList({ why = false }: { why?: boolean }) {
  return (
    <div className="space-y-1">
      {PERMISSIONS.map(([name, level, reason]) => (
        <div key={name} className="flex items-baseline gap-2 text-xs">
          <span className="text-slate-300 w-20 shrink-0">{name}</span>
          <span className="text-violet-300 w-24 shrink-0">{level}</span>
          {why && (
            <span className="text-slate-500 truncate" title={reason}>
              {reason}
            </span>
          )}
        </div>
      ))}
    </div>
  );
}

function ViewHeader({ title, onBack }: { title: string; onBack?: () => void }) {
  return (
    <div className="flex items-center gap-2">
      {onBack && (
        <button
          type="button"
          onClick={onBack}
          aria-label="Back"
          className="p-1 -ml-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
        >
          <ChevronLeft size={15} />
        </button>
      )}
      <h3 className="text-sm font-semibold text-slate-100">{title}</h3>
    </div>
  );
}

function Card({ children }: { children: ReactNode }) {
  return <div className="rounded-md border border-slate-700/50 bg-slate-900/40 p-4 space-y-4">{children}</div>;
}

export default function GitHubPanel() {
  const [connection, setConnection] = useState<GitHubConnection | null>(null);
  const [connections, setConnections] = useState<GitHubConnectionSummary[] | 'loading'>('loading');
  const [view, setView] = useState<View>('choose');
  const [busy, setBusy] = useState(false);
  const { showToast } = useToast();

  // Guided
  const [appName, setAppName] = useState('l8bin');
  const [customizeOpen, setCustomizeOpen] = useState(false);
  const [webhookUrl, setWebhookUrl] = useState(`${window.location.origin}/github/webhook`);
  const dashboardUrl = window.location.origin;

  // Manual attach
  const [appId, setAppId] = useState('');
  const [pem, setPem] = useState('');
  const [pemName, setPemName] = useState<string | null>(null);
  const [appWebhookSecret, setAppWebhookSecret] = useState('');

  // PAT
  const [token, setToken] = useState('');
  const [warnings, setWarnings] = useState<string[]>([]);

  const [confirmDisconnect, setConfirmDisconnect] = useState<number | null>(null);

  const [expandedInstall, setExpandedInstall] = useState<number | null>(null);
  const [installRepos, setInstallRepos] = useState<Record<number, GitHubRepo[] | 'loading' | 'error'>>({});

  const [dismissPending, setDismissPending] = useState(() => sessionStorage.getItem('ghPendingDismissed') === '1');

  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    if (!Array.isArray(connections)) return;
    const taken = new Set(connections.filter((c) => c.mode === 'app').map((c) => (c.label ?? '').toLowerCase()));
    let name = 'l8bin';
    for (let i = 2; taken.has(name); i++) name = `l8bin-${i}`;
    setAppName((cur) => (cur === 'l8bin' || /^l8bin-\d+$/.test(cur) ? name : cur));
  }, [connections]);

  const toggleInstallRepos = (installationId: number) => {
    setExpandedInstall((cur) => (cur === installationId ? null : installationId));
    setInstallRepos((cur) => {
      if (cur[installationId]) return cur;
      listGithubRepos(1, installationId)
        .then((page) => setInstallRepos((c) => ({ ...c, [installationId]: page.repos })))
        .catch(() => setInstallRepos((c) => ({ ...c, [installationId]: 'error' })));
      return { ...cur, [installationId]: 'loading' };
    });
  };

  const refresh = async () => {
    try {
      // Sync installations server-side, then read the snapshot.
      await listGithubInstallations().catch(() => {});
      const [conn, list] = await Promise.all([fetchGithubConnection(), listGithubConnections()]);
      setConnection(conn);
      setConnections(list);
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setLoaded(true);
    }
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: load once on mount
  useEffect(() => {
    refresh();
  }, []);

  // Keep installations fresh while the panel is open.
  useIntervalWhileVisible(() => {
    if (connection?.connected) refresh();
  }, 10000);

  const startAppFlow = async () => {
    setBusy(true);
    sessionStorage.removeItem('ghPendingDismissed');
    setDismissPending(false);
    try {
      const res = await startGithubManifest(appName.trim(), customizeOpen ? webhookUrl.trim() : undefined);
      // GitHub's documented prefill flow: POST the manifest as a form field to
      // the registration page. Same tab — the redirect lands on the callback
      // route, which finishes the flow.
      const form = document.createElement('form');
      form.method = 'POST';
      form.action = 'https://github.com/settings/apps/new';
      const field = document.createElement('input');
      field.type = 'hidden';
      field.name = 'manifest';
      field.value = res.manifest;
      form.appendChild(field);
      document.body.appendChild(form);
      form.submit();
    } catch (e) {
      showToast((e as Error).message, 'error');
      setBusy(false);
    }
  };

  const attachApp = async () => {
    const id = Number(appId.trim());
    if (!id || !pem.trim()) return;
    setBusy(true);
    try {
      const res = await connectGithubApp(id, pem.trim(), appWebhookSecret.trim() || undefined);
      setAppId('');
      setPem('');
      setPemName(null);
      setAppWebhookSecret('');
      setView('choose');
      showToast(`Attached GitHub App “${res.app_slug}”`, 'success');
      await refresh();
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const connectPat = async () => {
    if (!token.trim()) return;
    setBusy(true);
    setWarnings([]);
    try {
      const res = await connectGithubPat(token.trim());
      setToken('');
      setWarnings(res.warnings);
      showToast(`GitHub connected as ${res.login}`, 'success');
      await refresh();
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const doDisconnect = async (connectionId: number) => {
    setBusy(true);
    try {
      await disconnectGithub(connectionId);
      setConfirmDisconnect(null);
      showToast('Connection removed', 'success');
      await refresh();
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const connected = connection?.connected ?? false;

  // Hold a neutral state until the first fetch settles — no screen flashing.
  if (!loaded) {
    return (
      <div className="space-y-4">
        <h2 className="text-sm font-semibold text-slate-100">GitHub</h2>
        <Card>
          <div className="flex justify-center py-6">
            <Loader2 size={16} className="animate-spin text-slate-500" />
          </div>
        </Card>
      </div>
    );
  }

  // Setup screens take precedence so "+ Add another account" can open them.
  if (connection && connected && Array.isArray(connections) && view === 'choose') {
    const activeConnections = connections.filter((c) => c.status !== 'pending');
    const hasPending = connections.some((c) => c.status === 'pending');
    return (
      <div className="space-y-4">
        <h2 className="text-sm font-semibold text-slate-100">GitHub</h2>

        {hasPending && !dismissPending && (
          <div className="flex items-start gap-2">
            <p className="text-xs text-amber-300/90 flex-1">
              Unfinished setup — retry below, or{' '}
              <button
                type="button"
                onClick={() => setView('manual-attach')}
                className="underline underline-offset-2 hover:text-amber-200 transition-colors cursor-pointer"
              >
                attach the app
              </button>{' '}
              if GitHub already created it.
            </p>
            <button
              type="button"
              onClick={() => {
                sessionStorage.setItem('ghPendingDismissed', '1');
                setDismissPending(true);
              }}
              aria-label="Dismiss"
              className="p-0.5 rounded text-amber-300/60 hover:text-amber-200 transition-colors cursor-pointer shrink-0"
            >
              <X size={12} />
            </button>
          </div>
        )}

        {activeConnections.map((conn) => {
          const appMode = conn.mode === 'app';
          const degraded = conn.status === 'degraded';
          const installPending = appMode && conn.installations.length === 0;
          const chipLabel = degraded ? 'needs attention' : installPending ? 'created' : 'connected';
          return (
            <Card key={conn.id}>
              <div className="flex items-center gap-2">
                <GithubMark size={14} className="text-slate-300" />
                <span className="text-xs font-medium text-slate-200">{conn.label ?? 'GitHub'}</span>
                <span
                  className={`text-xs px-1.5 py-0.5 rounded ${
                    degraded
                      ? 'bg-amber-500/15 text-amber-300'
                      : installPending
                        ? 'bg-violet-500/15 text-violet-300'
                        : 'bg-emerald-500/15 text-emerald-300'
                  }`}
                >
                  {chipLabel}
                </span>
                <span className="text-xs px-1.5 py-0.5 rounded bg-slate-700/50 text-slate-400 uppercase">
                  {conn.mode}
                </span>
              </div>
              {degraded && conn.status_message && <p className="text-xs text-amber-300/90">{conn.status_message}</p>}

              {appMode &&
                (installPending ? (
                  <div className="space-y-3">
                    <Segments items={['Name it', 'Create on GitHub', 'Pick repos', 'Finish']} current={2} />
                    <p className="text-xs text-slate-400">
                      The app is created. One step left — install it on the GitHub account where your repos live, and
                      pick which ones it may see.
                    </p>
                    <a
                      href={`https://github.com/apps/${conn.label}/installations/new`}
                      target="_blank"
                      rel="noreferrer"
                      className="flex items-center justify-center gap-1.5 w-full py-2.5 rounded-md bg-violet-600 hover:bg-violet-500 text-xs font-medium text-white transition-colors"
                    >
                      Install on GitHub
                      <ExternalLink size={11} />
                    </a>
                  </div>
                ) : (
                  <div className="space-y-2">
                    {conn.installations.map((inst) => {
                      const selected = inst.repository_selection !== 'all';
                      const open = expandedInstall === inst.id;
                      const repos = installRepos[inst.id];
                      return (
                        <div key={inst.id} className="rounded-md border border-slate-700/50 bg-slate-900/60">
                          <div className="flex items-center gap-3 p-3">
                            <button
                              type="button"
                              disabled={!selected}
                              onClick={() => toggleInstallRepos(inst.id)}
                              className={`flex items-center gap-3 flex-1 min-w-0 text-left ${
                                selected ? 'cursor-pointer' : 'cursor-default'
                              }`}
                            >
                              {inst.account_avatar ? (
                                <img src={inst.account_avatar} alt="" className="h-8 w-8 rounded-full shrink-0" />
                              ) : (
                                <span className="flex items-center justify-center h-8 w-8 rounded-full bg-slate-800 shrink-0">
                                  <GithubMark size={14} />
                                </span>
                              )}
                              <span className="flex-1 min-w-0">
                                <span className="block text-sm font-medium text-slate-200 truncate">
                                  {inst.account_login}
                                </span>
                                <span className="block text-xs text-slate-500">
                                  {selected ? (
                                    <>
                                      Show selected repos
                                      <ChevronDown
                                        size={11}
                                        className={`inline-block transition-transform ${open ? 'rotate-180' : ''}`}
                                      />
                                    </>
                                  ) : (
                                    'Access to all repositories'
                                  )}
                                </span>
                              </span>
                            </button>
                            {inst.status !== 'active' && (
                              <span className="text-xs text-amber-300 shrink-0">{inst.status}</span>
                            )}
                            <a
                              href={`https://github.com/apps/${conn.label}/installations/new`}
                              target="_blank"
                              rel="noreferrer"
                              className="text-xs text-violet-400 hover:text-violet-300 transition-colors shrink-0"
                            >
                              Change repos
                            </a>
                          </div>
                          {open && (
                            <div className="px-3 pb-3 space-y-1">
                              {repos === undefined || repos === 'loading' ? (
                                <p className="text-xs text-slate-500 flex items-center gap-1.5">
                                  <Loader2 size={11} className="animate-spin" />
                                  Loading repositories…
                                </p>
                              ) : repos === 'error' ? (
                                <p className="text-xs text-amber-300/90">Couldn’t load repositories.</p>
                              ) : (
                                repos.map((repo) => (
                                  <a
                                    key={repo.id}
                                    href={repo.html_url}
                                    target="_blank"
                                    rel="noreferrer"
                                    className="flex items-center gap-2 rounded-md border border-slate-700/40 bg-slate-900/50 px-2.5 py-1.5 text-xs text-slate-300 hover:border-violet-500/40 hover:bg-slate-900 hover:text-slate-100 transition-colors"
                                  >
                                    {repo.private ? (
                                      <Lock size={10} className="text-slate-600 shrink-0" />
                                    ) : (
                                      <GithubMark size={10} className="text-slate-600 shrink-0" />
                                    )}
                                    <span className="truncate flex-1">{repo.full_name}</span>
                                    <ExternalLink size={10} className="text-slate-600 shrink-0" />
                                  </a>
                                ))
                              )}
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>
                ))}

              {confirmDisconnect === conn.id ? (
                <div className="space-y-2 pt-2 border-t border-slate-700/40">
                  <p className="text-xs text-slate-400">
                    {appMode
                      ? 'Deploys from this app stop. To use it again you’ll re-attach its ID + key — or create a new one.'
                      : 'Deploys from this token stop. Reconnecting is a paste away.'}
                  </p>
                  <div className="flex gap-2">
                    <button
                      type="button"
                      onClick={() => doDisconnect(conn.id)}
                      disabled={busy}
                      className="px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 disabled:opacity-50 text-xs font-medium text-white transition-colors cursor-pointer"
                    >
                      {busy ? <Loader2 size={11} className="animate-spin" /> : 'Disconnect'}
                    </button>
                    <button
                      type="button"
                      onClick={() => setConfirmDisconnect(null)}
                      className="px-3 py-1.5 rounded-md border border-slate-700/50 hover:border-slate-600 text-xs text-slate-400 transition-colors cursor-pointer"
                    >
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <div className="flex items-center gap-3 text-xs">
                  {appMode && conn.label && (
                    <>
                      <a
                        href={`https://github.com/settings/apps/${conn.label}`}
                        target="_blank"
                        rel="noreferrer"
                        className="flex items-center gap-1 text-slate-500 hover:text-slate-300 transition-colors"
                      >
                        Manage the app
                        <ExternalLink size={10} />
                      </a>
                      <span className="text-slate-700">|</span>
                    </>
                  )}
                  <button
                    type="button"
                    onClick={() => setConfirmDisconnect(conn.id)}
                    className="text-red-400 hover:text-red-300 transition-colors cursor-pointer"
                  >
                    Disconnect
                  </button>
                </div>
              )}
            </Card>
          );
        })}

        <button
          type="button"
          onClick={() => setView('guided')}
          className="flex items-center justify-center gap-1.5 w-full py-2 rounded-md border border-dashed border-slate-600/70 text-xs text-slate-500 hover:text-slate-300 hover:border-slate-500 transition-colors cursor-pointer"
        >
          <Plus size={12} />
          Add another GitHub account
        </button>
      </div>
    );
  }

  // Choose a connection path.
  if (view === 'choose') {
    return (
      <div className="space-y-4">
        <h2 className="text-sm font-semibold text-slate-100">Connect GitHub</h2>

        {/* The Flow — what's going to happen */}
        <Card>
          <div className="flex items-center gap-2">
            <Workflow size={13} className="text-violet-400 shrink-0" />
            <span className="text-xs font-semibold text-slate-200 tracking-wide">The Flow</span>
            <span className="flex-1 h-px bg-slate-700/50" />
          </div>
          <Segments items={['Name it', 'Create on GitHub', 'Pick repos', 'Finish']} />
          <p className="text-sm text-slate-300">
            Deploy straight from your repos. Your own private GitHub App makes it seamless.
          </p>
        </Card>

        {/* Actions */}
        <Card>
          {connection?.status === 'pending' && !dismissPending && (
            <div className="flex items-start gap-2">
              <p className="text-xs text-amber-300/90 flex-1">
                Unfinished setup — retry Connect, or{' '}
                <button
                  type="button"
                  onClick={() => setView('manual-attach')}
                  className="underline underline-offset-2 hover:text-amber-200 transition-colors cursor-pointer"
                >
                  attach the app
                </button>{' '}
                if GitHub already created it.
              </p>
              <button
                type="button"
                onClick={() => {
                  sessionStorage.setItem('ghPendingDismissed', '1');
                  setDismissPending(true);
                }}
                aria-label="Dismiss"
                className="p-0.5 rounded text-amber-300/60 hover:text-amber-200 transition-colors cursor-pointer shrink-0"
              >
                <X size={12} />
              </button>
            </div>
          )}

          <button
            type="button"
            onClick={() => setView('guided')}
            className="flex items-center gap-3 w-full p-3 rounded-md border border-violet-500/40 bg-violet-500/10 hover:bg-violet-500/15 text-left transition-colors cursor-pointer"
          >
            <GithubMark size={18} className="text-violet-300 shrink-0" />
            <span className="flex-1">
              <span className="block text-xs font-medium text-slate-100">Create GitHub App</span>
              <span className="block text-xs text-slate-400">Recommended · ~2 min</span>
            </span>
            <ArrowRight size={13} className="text-violet-300" />
          </button>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
            <button
              type="button"
              onClick={() => setView('manual-setup')}
              className="flex items-center gap-2 p-2.5 rounded-md border border-slate-700/50 hover:border-slate-600 text-left transition-colors cursor-pointer"
            >
              <FileKey2 size={14} className="text-slate-400 shrink-0" />
              <span className="text-xs text-slate-300 flex-1">Create the app myself</span>
              <ArrowRight size={12} className="text-slate-600 shrink-0" />
            </button>
            <button
              type="button"
              onClick={() => setView('pat')}
              className="flex items-center gap-2 p-2.5 rounded-md border border-slate-700/50 hover:border-slate-600 text-left transition-colors cursor-pointer"
            >
              <KeyRound size={14} className="text-slate-400 shrink-0" />
              <span className="text-xs text-slate-300 flex-1">Use a token</span>
              <ArrowRight size={12} className="text-slate-600 shrink-0" />
            </button>
          </div>
        </Card>
      </div>
    );
  }

  // Guided flow: name the app, then continue on GitHub.
  if (view === 'guided') {
    return (
      <div className="space-y-4">
        <Card>
          <ViewHeader title="Create GitHub App" onBack={() => setView('choose')} />

          <div>
            <label htmlFor="gh-app-name" className="block text-xs font-medium text-slate-400 mb-1.5">
              App name
            </label>
            <div className="flex gap-2">
              <input
                id="gh-app-name"
                type="text"
                value={appName}
                onChange={(e) => setAppName(e.target.value)}
                maxLength={34}
                placeholder="l8bin"
                className={`${inputClass} flex-1`}
              />
              <button
                type="button"
                onClick={startAppFlow}
                disabled={busy || !appName.trim()}
                className={`${buttonBase} shrink-0`}
              >
                {busy ? <Loader2 size={12} className="animate-spin" /> : <GithubMark size={12} />}
                Continue to GitHub
              </button>
            </div>
          </div>

          <Disclosure summary="Customize" open={customizeOpen} onToggle={() => setCustomizeOpen((v) => !v)}>
            <div>
              <label htmlFor="gh-webhook" className="block text-xs font-medium text-slate-400 mb-1.5">
                Webhook URL
              </label>
              <input
                id="gh-webhook"
                type="text"
                value={webhookUrl}
                onChange={(e) => setWebhookUrl(e.target.value)}
                title="Must be reachable from GitHub"
                className={`${inputClass} font-mono text-xs`}
              />
            </div>
            <div>
              <span className="block text-xs font-medium text-slate-400 mb-1.5">Redirect URL</span>
              <CopyRow label="Redirect" value={`${dashboardUrl}/settings/github/callback`} />
            </div>
          </Disclosure>

          <Disclosure summary="What permissions does it get?">
            <PermissionsList why />
            <p className="text-xs text-slate-500">Private app · no event subscriptions</p>
          </Disclosure>
        </Card>
      </div>
    );
  }

  // Manual flow, step 1: create the app on GitHub.
  if (view === 'manual-setup') {
    return (
      <div className="space-y-4">
        <Card>
          <ViewHeader title="Create the app myself" onBack={() => setView('choose')} />
          <Segments items={['Create on GitHub', 'Attach here']} current={0} />

          <p className="text-xs text-slate-500">
            On{' '}
            <a
              href="https://github.com/settings/apps/new"
              target="_blank"
              rel="noreferrer"
              className="text-violet-400 hover:text-violet-300 inline-flex items-center gap-0.5"
            >
              github.com/settings/apps/new
              <ExternalLink size={10} />
            </a>
            , use:
          </p>

          <div className="rounded-md border border-dashed border-slate-600/70 p-3 space-y-1.5">
            <CopyRow label="Name" value="l8bin" />
            <CopyRow label="Homepage" value={dashboardUrl} />
            <CopyRow label="Webhook" value={`${dashboardUrl}/github/webhook`} />
          </div>

          <Disclosure summary="Repository permissions">
            <PermissionsList />
          </Disclosure>
          <p className="text-xs text-slate-500">No events · keep it private.</p>

          <button type="button" onClick={() => setView('manual-attach')} className={buttonPrimary}>
            I created it — next
            <ArrowRight size={12} />
          </button>
        </Card>
      </div>
    );
  }

  // Manual flow, step 2: attach the created app.
  if (view === 'manual-attach') {
    return (
      <div className="space-y-4">
        <Card>
          <ViewHeader title="Attach the app" onBack={() => setView('manual-setup')} />
          <Segments items={['Create on GitHub', 'Attach here']} current={1} />

          <p className="text-xs text-slate-500">
            From the app’s GitHub page: <span className="text-slate-300">About → App ID</span>, and{' '}
            <span className="text-slate-300">Private keys → Generate</span>.
          </p>

          <input
            type="number"
            value={appId}
            onChange={(e) => setAppId(e.target.value)}
            placeholder="App ID"
            className={inputClass}
          />
          <textarea
            value={pem}
            onChange={(e) => {
              setPem(e.target.value);
              setPemName(null);
            }}
            placeholder="-----BEGIN RSA PRIVATE KEY----- … -----END RSA PRIVATE KEY-----"
            rows={4}
            spellCheck={false}
            className={`${inputClass} text-xs font-mono resize-y`}
          />
          <div>
            <label className="text-xs text-violet-400 hover:text-violet-300 transition-colors cursor-pointer">
              {pemName ? `✓ ${pemName}` : 'Pick the .pem file instead'}
              <input
                type="file"
                accept=".pem,key"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  if (!file) return;
                  file.text().then((text) => {
                    setPem(text);
                    setPemName(file.name);
                  });
                }}
              />
            </label>
          </div>
          <input
            type="password"
            value={appWebhookSecret}
            onChange={(e) => setAppWebhookSecret(e.target.value)}
            placeholder="Webhook secret (if you set one)"
            className={inputClass}
          />

          <button
            type="button"
            onClick={attachApp}
            disabled={busy || !appId.trim() || !pem.trim()}
            className={buttonPrimary}
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            Attach app
          </button>
        </Card>
      </div>
    );
  }

  // PAT connection.
  return (
    <div className="space-y-4">
      <Card>
        <ViewHeader title="Personal access token" onBack={() => setView('choose')} />

        <p className="text-xs text-slate-500">
          Needs <code className="text-slate-400">repo</code> + <code className="text-slate-400">workflow</code> scopes ·{' '}
          <a
            href="https://github.com/settings/tokens/new?scopes=repo,workflow&description=l8bin%20deploy"
            target="_blank"
            rel="noreferrer"
            className="text-violet-400 hover:text-violet-300 inline-flex items-center gap-0.5"
          >
            create one
            <ExternalLink size={10} />
          </a>
        </p>

        <div className="flex gap-2">
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder="ghp_… or github_pat_…"
            className={inputClass}
          />
          <button
            type="button"
            onClick={connectPat}
            disabled={busy || !token.trim()}
            className={`${buttonBase} shrink-0`}
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            Connect
          </button>
        </div>

        {warnings.map((w) => (
          <p key={w} className="text-xs text-amber-300/90 flex items-start gap-1.5">
            <AlertTriangle size={12} className="mt-0.5 shrink-0" />
            {w}
          </p>
        ))}
      </Card>
    </div>
  );
}
