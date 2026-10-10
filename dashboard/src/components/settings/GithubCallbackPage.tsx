import { AlertTriangle, CheckCircle2, ExternalLink, Loader2 } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { exchangeGithubManifest, listGithubConnections, listGithubInstallations } from '../../api';

/**
 * Manifest-flow landing: GitHub redirects here with ?code= after the user
 * confirms the prefilled app registration. We exchange it, then walk the
 * user through installing the app on their account.
 */
export default function GithubCallbackPage() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const code = params.get('code');
  const [phase, setPhase] = useState<'exchanging' | 'install' | 'error'>('exchanging');
  const [installUrl, setInstallUrl] = useState<string | null>(null);
  const [manageUrl, setManageUrl] = useState<string | null>(null);
  const [appSlug, setAppSlug] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Manifest codes are single-use: never exchange twice for one landing
  // (StrictMode fires effects twice in dev; refresh replays the code too —
  // the server tolerates that, but no reason to burn a second request).
  const exchanged = useRef(false);

  // biome-ignore lint/correctness/useExhaustiveDependencies: one-shot on landing; code/params are read once
  useEffect(() => {
    if (exchanged.current) return;
    exchanged.current = true;

    // GitHub redirects with ?error=… when the manifest flow fails or is
    // cancelled on their side — no code to exchange then.
    const ghError = params.get('error_description') ?? params.get('error');
    if (ghError) {
      setError(ghError);
      setPhase('error');
      return;
    }
    if (!code) {
      setError('missing code parameter — start again from Settings → GitHub');
      setPhase('error');
      return;
    }
    (async () => {
      try {
        const res = await exchangeGithubManifest(code);
        setAppSlug(res.app_slug);
        setInstallUrl(res.install_url);
        setManageUrl(`https://github.com/settings/apps/${res.app_slug}`);
        setPhase('install');
      } catch (e) {
        setError((e as Error).message);
        setPhase('error');
      }
    })();
  }, []);

  // Poll for THIS app's installation appearing (user completed the install).
  // Scoped by slug — other apps' installations must not satisfy the wait.
  useEffect(() => {
    if (phase !== 'install' || !appSlug) return;
    const id = setInterval(async () => {
      try {
        const [installations, connections] = await Promise.all([listGithubInstallations(), listGithubConnections()]);
        const conn = connections.find((c) => c.mode === 'app' && c.label === appSlug);
        const done = !!conn && installations.some((i) => i.connection_id === conn.id && i.status === 'active');
        if (done) {
          clearInterval(id);
          navigate('/settings/github', { state: { refresh: Date.now() } });
        }
      } catch {
        // Not installed yet — keep waiting.
      }
    }, 3000);
    return () => clearInterval(id);
  }, [phase, appSlug, navigate]);

  return (
    <div className="min-h-screen bg-slate-950 text-slate-200 flex items-center justify-center p-6">
      <div className="w-full max-w-md rounded-lg border border-slate-700/50 bg-slate-900/40 p-5 space-y-4">
        {phase === 'exchanging' && (
          <>
            <div className="flex justify-center">
              <Loader2 size={20} className="animate-spin text-violet-400" />
            </div>
            <p className="text-xs text-slate-400 text-center">Creating your GitHub App…</p>
          </>
        )}

        {phase === 'install' && (
          <>
            <div className="flex items-center gap-2">
              <CheckCircle2 size={15} className="text-emerald-400 shrink-0" />
              <h2 className="text-sm font-semibold text-emerald-300">
                Created Your Own Private GitHub App “{appSlug}”
              </h2>
            </div>

            <p className="text-xs text-slate-400">
              Next: install it on the GitHub account where your repos live.
            </p>

            {/* Screenshot of GitHub's install screen so it looks familiar. */}
            <div className="space-y-2">
              <img
                src="/github-app-install.png"
                alt="GitHub app install screen: choose all or select repositories, then press Install"
                className="w-full rounded-md shadow-lg shadow-black/40 ring-1 ring-slate-700/60"
              />
              <p className="text-xs text-slate-500">
                Pick which repositories it may see, then press <span className="text-slate-300">Install</span>.
              </p>
            </div>

            {installUrl && (
              <a
                href={installUrl}
                target="_blank"
                rel="noreferrer"
                className="flex items-center justify-center gap-1.5 w-full py-2.5 rounded-md bg-violet-600 hover:bg-violet-500 text-xs font-medium text-white transition-colors"
              >
                Install on GitHub
                <ExternalLink size={11} />
              </a>
            )}
            {manageUrl && (
              <a
                href={manageUrl}
                target="_blank"
                rel="noreferrer"
                className="flex items-center justify-center gap-1 text-xs text-slate-500 hover:text-slate-300 transition-colors"
              >
                Manage the app
                <ExternalLink size={10} />
              </a>
            )}

            <div className="flex items-center justify-between text-xs">
              <span className="flex items-center gap-1.5 text-slate-500">
                <Loader2 size={11} className="animate-spin" />
                waiting for installation…
              </span>
              <button
                type="button"
                onClick={() => navigate('/settings/github')}
                className="text-slate-500 hover:text-slate-400 transition-colors cursor-pointer"
              >
                Skip — later
              </button>
            </div>
          </>
        )}

        {phase === 'error' && (
          <>
            <div className="flex items-start gap-2">
              <AlertTriangle size={14} className="text-amber-400 mt-0.5 shrink-0" />
              <p className="text-xs text-red-400">{error}</p>
            </div>
            <button
              type="button"
              onClick={() => navigate('/settings/github')}
              className="w-full py-2 rounded-md border border-slate-700/50 hover:border-slate-600 text-xs text-slate-300 transition-colors cursor-pointer"
            >
              Back to settings
            </button>
          </>
        )}
      </div>
    </div>
  );
}
