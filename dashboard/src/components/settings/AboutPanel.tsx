import { Copy, ExternalLink, RefreshCw, Star } from 'lucide-react';
import { useEffect, useState } from 'react';
import { fetchLatestVersion, fetchVersion, type LatestVersionInfo } from '../../api';
import GithubMark from '../GithubMark';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

const REPO_URL = 'https://github.com/mtsandeep/l8bin';
const RELEASES_URL = 'https://github.com/mtsandeep/l8bin/releases';
const ISSUES_URL = 'https://github.com/mtsandeep/l8bin/issues';
const WEBSITE_URL = 'https://l8bin.com';
const INSTALL_CMD = 'curl -fsSL https://l8b.in | bash';
const UPDATE_CMD = 'curl -fsSL https://l8b.in | bash -s update';

/** Numeric segment compare: 0.4.9 < 0.4.10 < 0.5.0. */
// Semver compare incl. pre-releases (0.5.0-beta.1 < 0.5.0): compare cores
// numerically, then a pre-release sorts before its own release.
function compareVersions(a: string, b: string): number {
  const parse = (v: string) => {
    const [core, pre] = v.replace(/^v/, '').split(/-(.+)/);
    return { core: core.split('.').map(Number), pre: pre?.split('.') ?? null };
  };
  const va = parse(a);
  const vb = parse(b);
  for (let i = 0; i < Math.max(va.core.length, vb.core.length); i++) {
    const d = (va.core[i] || 0) - (vb.core[i] || 0);
    if (d !== 0) return d;
  }
  if (!va.pre && !vb.pre) return 0;
  if (!va.pre) return 1;
  if (!vb.pre) return -1;
  for (let i = 0; i < Math.max(va.pre.length, vb.pre.length); i++) {
    const x = va.pre[i];
    const y = vb.pre[i];
    if (x === undefined) return -1; // 0.5.0-beta < 0.5.0-beta.1
    if (y === undefined) return 1;
    const nx = Number(x);
    const ny = Number(y);
    if (!Number.isNaN(nx) && !Number.isNaN(ny)) {
      if (nx !== ny) return nx - ny;
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

function CommandBlock({ command, onCopy }: { command: string; onCopy: () => void }) {
  return (
    <div className="relative bg-slate-950 border border-slate-700/60 rounded-lg p-3">
      <pre className="text-xs text-emerald-400 font-mono whitespace-pre-wrap break-all pr-8">{command}</pre>
      <button
        type="button"
        onClick={onCopy}
        className="absolute right-2 top-2.5 text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
        title="Copy"
      >
        <Copy size={13} />
      </button>
    </div>
  );
}

export default function AboutPanel() {
  const { showToast } = useToast();
  const [version, setVersion] = useState('');
  const [latest, setLatest] = useState<LatestVersionInfo | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [v, l] = await Promise.all([fetchVersion(), fetchLatestVersion()]);
      if (cancelled) return;
      setVersion(v);
      setLatest(l);
      setLoading(false);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const copy = async (text: string, label: string) => {
    await navigator.clipboard.writeText(text);
    showToast(`${label} copied`, 'success');
  };

  const latestVersion = latest?.latest_version ?? null;
  const updateAvailable = !!latestVersion && !!version && compareVersions(latestVersion, version) > 0;

  if (loading) {
    return (
      <div className="flex items-center justify-center py-8">
        <div className="w-4 h-4 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <SettingsCard title="About LiteBin">
        <div className="flex justify-center mb-4">
          <img src="/logo.svg" alt="" className="h-10 w-auto" style={{ height: 40 }} />
        </div>

        <div className="space-y-2 text-xs">
          <div className="flex items-center justify-between gap-3">
            <span className="text-slate-500">Running</span>
            <span className="flex items-center gap-2 min-w-0">
              <span className="font-mono text-slate-200">{version ? `v${version}` : '—'}</span>
              {version?.includes('-') && (
                <span className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-amber-500/10 text-amber-300 border border-amber-500/30">
                  pre-release
                </span>
              )}
            </span>
          </div>
          <div className="flex items-center justify-between gap-3">
            <span className="text-slate-500">Latest release</span>
            <span className="flex items-center gap-2 min-w-0">
              {latestVersion ? (
                <>
                  <span className="font-mono text-slate-200">v{latestVersion}</span>
                  {updateAvailable ? (
                    <span className="shrink-0 px-2 py-0.5 rounded-md text-[10px] font-medium bg-amber-500/15 text-amber-300 border border-amber-500/25">
                      Update available
                    </span>
                  ) : (
                    version && (
                      <span className="shrink-0 px-2 py-0.5 rounded-md text-[10px] font-medium bg-emerald-500/15 text-emerald-300 border border-emerald-500/25">
                        Up to date
                      </span>
                    )
                  )}
                </>
              ) : (
                <span className="text-slate-500">couldn&apos;t check — is this server offline?</span>
              )}
            </span>
          </div>
        </div>

        <div className="flex items-center gap-4 mt-4 pt-3 border-t border-slate-700/40 text-xs">
          <a
            href={latest?.release_url ?? RELEASES_URL}
            target="_blank"
            rel="noreferrer"
            className="flex items-center gap-1 text-slate-500 hover:text-slate-300 transition-colors"
          >
            Release notes <ExternalLink size={10} />
          </a>
          <a
            href={WEBSITE_URL}
            target="_blank"
            rel="noreferrer"
            className="flex items-center gap-1 text-slate-500 hover:text-slate-300 transition-colors"
          >
            l8bin.com <ExternalLink size={10} />
          </a>
          <a
            href={ISSUES_URL}
            target="_blank"
            rel="noreferrer"
            className="flex items-center gap-1 text-slate-500 hover:text-slate-300 transition-colors"
          >
            Request a feature <ExternalLink size={10} />
          </a>
        </div>

        <div className="mt-4 pt-3 border-t border-slate-700/40">
          <div className="flex items-center justify-between gap-3 mb-3">
            <span className="text-xs text-slate-500">Source code</span>
            <span className="flex items-center gap-2 min-w-0">
              <a
                href={REPO_URL}
                target="_blank"
                rel="noreferrer"
                className="font-mono text-xs text-slate-300 hover:text-slate-100 truncate"
              >
                {REPO_URL.replace('https://', '')}
              </a>
              <button
                type="button"
                onClick={() => copy(REPO_URL, 'Repository URL')}
                className="shrink-0 text-slate-500 hover:text-slate-300 transition-colors cursor-pointer"
                title="Copy"
              >
                <Copy size={13} />
              </button>
            </span>
          </div>

          <a
            href={REPO_URL}
            target="_blank"
            rel="noreferrer"
            className="flex items-center justify-center gap-2 w-full py-2.5 rounded-md bg-slate-800 text-slate-200 hover:bg-slate-700 border border-slate-700/60 transition-colors"
          >
            <GithubMark size={14} />
            Star l8bin on GitHub
            <Star size={13} className="text-amber-400" />
          </a>
          <p className="text-[11px] text-slate-500 text-center mt-2">
            If LiteBin is useful to you, a star is the easiest thanks.
          </p>
        </div>
      </SettingsCard>

      <SettingsCard icon={RefreshCw} title="Upgrade">
        <CommandBlock command={INSTALL_CMD} onCopy={() => copy(INSTALL_CMD, 'Installer command')} />
        <p className="text-[11px] text-slate-500 mt-2">
          Follow the interactive installer and pick 5 for update — or run it directly:{' '}
          <span className="font-mono text-slate-400">{UPDATE_CMD}</span>
        </p>
      </SettingsCard>
    </div>
  );
}
