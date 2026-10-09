import { AlertTriangle, CheckCircle2, ExternalLink, Loader2 } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { exchangeGithubManifest, type GitHubInstallation, listGithubInstallations } from '../../api';

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
        setPhase('install');
      } catch (e) {
        setError((e as Error).message);
        setPhase('error');
      }
    })();
  }, []);

  // Poll for the installation appearing (user completed the install flow).
  useEffect(() => {
    if (phase !== 'install') return;
    const id = setInterval(async () => {
      try {
        const installations: GitHubInstallation[] = await listGithubInstallations();
        if (installations.some((i) => i.status === 'active')) {
          clearInterval(id);
          navigate('/settings/github', { state: { refresh: Date.now() } });
        }
      } catch {
        // Not installed yet — keep waiting.
      }
    }, 3000);
    return () => clearInterval(id);
  }, [phase, navigate]);

  return (
    <div className="min-h-screen bg-slate-950 text-slate-200 flex items-center justify-center p-6">
      <div className="w-full max-w-md rounded-lg border border-slate-700/50 bg-slate-800 p-6 space-y-4">
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
              <CheckCircle2 size={14} className="text-emerald-400" />
              <p className="text-xs text-slate-200 font-medium">GitHub App “{appSlug}” created</p>
            </div>
            <p className="text-xs text-slate-500">
              One more step — install it on your GitHub account and pick which repositories it may see. This page will
              continue automatically once the install completes.
            </p>
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
            <div className="flex items-center gap-2 text-xs text-slate-500">
              <Loader2 size={11} className="animate-spin" />
              waiting for installation…
            </div>
            <button
              type="button"
              onClick={() => navigate('/settings/github')}
              className="w-full text-xs text-slate-500 hover:text-slate-400 transition-colors cursor-pointer"
            >
              Skip — I’ll install later
            </button>
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
