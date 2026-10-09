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
  type GitHubInstallation,
  listGithubInstallations,
  startGithubManifest,
} from '../../api';
import GithubMark from '../GithubMark';
import { useToast } from '../ToastContext';

/** Full-replacement screens — no nested accordions. */
type View = 'choose' | 'guided' | 'manual-setup' | 'manual-attach' | 'pat';

const inputClass =
  'w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors';

const buttonBase =
  'flex items-center justify-center gap-1.5 py-2.5 px-4 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 disabled:hover:bg-violet-600 text-xs font-medium text-white transition-colors cursor-pointer';

const buttonPrimary = `${buttonBase} w-full`;

/** Chevron-ribbon stepper: interlocking arrow segments, numbered inside.
 *  Segments before `current` are done (✓), `current` is highlighted. */
function Segments({ items, current = 0 }: { items: string[]; current?: number }) {
  return (
    <div className="flex w-full">
      {items.map((label, i) => {
        const done = i < current;
        const active = i === current;
        // Every segment points right; later segments also notch left so the
        // pieces interlock like a ribbon.
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

/** Quiet "what's this?" toggle — the details never block the primary action.
 *  Controlled via open/onToggle when the parent needs to know (Customize). */
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

/** Plain label + value + copy row (no field chrome) — used inside the
 *  dashed reference box on the manual screen and under Customize. */
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
  const [installations, setInstallations] = useState<GitHubInstallation[] | 'loading' | 'error'>('loading');
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

  const [confirmDisconnect, setConfirmDisconnect] = useState(false);

  // "Unfinished setup" hint stays dismissed (across navigation) until the
  // user starts a fresh Connect.
  const [dismissPending, setDismissPending] = useState(() => sessionStorage.getItem('ghPendingDismissed') === '1');

  const refresh = async () => {
    try {
      const conn = await fetchGithubConnection();
      setConnection(conn);
      if (conn.mode === 'app' && conn.connected) {
        try {
          setInstallations(await listGithubInstallations());
        } catch {
          setInstallations('error');
        }
      } else {
        setInstallations('loading');
      }
    } catch (e) {
      showToast((e as Error).message, 'error');
    }
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: load once on mount
  useEffect(() => {
    refresh();
  }, []);

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

  const doDisconnect = async () => {
    setBusy(true);
    try {
      await disconnectGithub();
      setConfirmDisconnect(false);
      setView('choose');
      showToast('GitHub disconnected', 'success');
      await refresh();
    } catch (e) {
      showToast((e as Error).message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const connected = connection?.connected ?? false;
  const degraded = connection?.status === 'degraded';
  const appMode = connection?.mode === 'app';

  // ── Connected ──────────────────────────────────────────────────────────────
  if (connection && connected) {
    return (
      <div className="space-y-4">
        <h2 className="text-sm font-semibold text-slate-100">GitHub</h2>

        <Card>
          <div className="flex items-center gap-2">
            <GithubMark size={14} className="text-slate-300" />
            <span className="text-xs font-medium text-slate-200">{connection.login}</span>
            <span
              className={`text-xs px-1.5 py-0.5 rounded ${
                degraded ? 'bg-amber-500/15 text-amber-300' : 'bg-emerald-500/15 text-emerald-300'
              }`}
            >
              {degraded ? 'needs attention' : 'connected'}
            </span>
            <span className="text-xs px-1.5 py-0.5 rounded bg-slate-700/50 text-slate-400 uppercase">
              {connection.mode}
            </span>
          </div>
          {degraded && connection.status_message && (
            <p className="text-xs text-amber-300/90">{connection.status_message}</p>
          )}

          {appMode && (
            <div className="space-y-1.5">
              {installations === 'loading' ? (
                <p className="text-xs text-slate-500">Loading…</p>
              ) : installations === 'error' ? (
                <p className="text-xs text-amber-300/90">Couldn’t load installations.</p>
              ) : installations.length === 0 ? (
                <p className="text-xs text-amber-300/90">Not installed on any account yet.</p>
              ) : (
                installations.map((inst) => (
                  <div key={inst.id} className="flex items-center gap-2 text-xs text-slate-400">
                    {inst.account_avatar ? (
                      <img src={inst.account_avatar} alt="" className="h-4 w-4 rounded-full" />
                    ) : (
                      <GithubMark size={11} />
                    )}
                    <span className="text-slate-300">{inst.account_login}</span>
                    <span>{inst.repository_selection === 'all' ? 'all repos' : 'selected repos'}</span>
                    {inst.status !== 'active' && <span className="text-amber-300">{inst.status}</span>}
                  </div>
                ))
              )}
              {connection.login && (
                <a
                  href={`https://github.com/apps/${connection.login}/installations/new`}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 text-xs text-violet-400 hover:text-violet-300 transition-colors"
                >
                  <Plus size={10} />
                  Add or change repositories
                  <ExternalLink size={10} />
                </a>
              )}
            </div>
          )}

          {confirmDisconnect ? (
            <div className="space-y-2 pt-2 border-t border-slate-700/40">
              <p className="text-xs text-slate-400">
                Deploys from GitHub stop. To reconnect this app you’ll re-enter its ID + key — or create a new one.
              </p>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={doDisconnect}
                  disabled={busy}
                  className="px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 disabled:opacity-50 text-xs font-medium text-white transition-colors cursor-pointer"
                >
                  {busy ? <Loader2 size={11} className="animate-spin" /> : 'Disconnect'}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmDisconnect(false)}
                  className="px-3 py-1.5 rounded-md border border-slate-700/50 hover:border-slate-600 text-xs text-slate-400 transition-colors cursor-pointer"
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setConfirmDisconnect(true)}
              className="text-xs text-red-400 hover:text-red-300 transition-colors cursor-pointer"
            >
              Disconnect
            </button>
          )}
        </Card>
      </div>
    );
  }

  // ── Choose ─────────────────────────────────────────────────────────────────
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

  // ── Guided ─────────────────────────────────────────────────────────────────
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

  // ── Manual: create ─────────────────────────────────────────────────────────
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

          {/* Reference values for GitHub's form — dashed box says "take these". */}
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

  // ── Manual: attach ─────────────────────────────────────────────────────────
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

  // ── PAT ────────────────────────────────────────────────────────────────────
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
