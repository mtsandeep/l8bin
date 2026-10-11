import { AlertTriangle, CheckCircle2, ChevronLeft, KeyRound, Loader2, Rocket, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import {
  createProject,
  dispatchGithubDeploy,
  type GitHubRepo,
  type GitHubRepoDetail,
  getGithubRepoDetail,
  getProjectGithubConfig,
  linkProjectGithub,
  listGithubBranches,
  type Project,
} from '../api';
import GithubMark from './GithubMark';
import { useToast } from './ToastContext';

interface Props {
  repo: GitHubRepo;
  domain: string;
  installationId?: number;
  connectionId?: number;
  /** Link mode: the project is fixed (existing project), ID not editable. */
  fixedProjectId?: string;
  onBack: () => void;
  onClose: () => void;
  onLinked: () => void;
}

function sanitizeProjectId(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, '')
    .slice(0, 63);
}

export default function GitHubDeployForm({
  repo,
  domain,
  installationId,
  connectionId,
  fixedProjectId,
  onBack,
  onClose,
  onLinked,
}: Props) {
  const [owner, repoName] = useMemo(() => repo.full_name.split('/'), [repo.full_name]);
  const [detail, setDetail] = useState<GitHubRepoDetail | null>(null);
  const [branches, setBranches] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [projectId, setProjectId] = useState(sanitizeProjectId(repoName));
  const [branch, setBranch] = useState('');
  const [repoPath, setRepoPath] = useState('');
  const [folderStatus, setFolderStatus] = useState<'idle' | 'checking' | 'ok' | 'not-found'>('idle');
  const [idTaken, setIdTaken] = useState(false);
  const [deployKind, setDeployKind] = useState<'single' | 'compose'>('single');
  const [port, setPort] = useState(3000);
  const [autoDeploy, setAutoDeploy] = useState(true);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [commitMessage, setCommitMessage] = useState('LiteBin: add deploy workflow');
  const { showToast } = useToast();

  useEffect(() => {
    (async () => {
      setLoading(true);
      try {
        const [d, b] = await Promise.all([
          getGithubRepoDetail(owner, repoName, undefined, installationId, connectionId),
          listGithubBranches(owner, repoName, installationId, connectionId),
        ]);
        setDetail(d);
        setBranches(b.map((x) => x.name));
        setBranch(d.default_branch);
        setDeployKind(d.kind);
      } catch (e) {
        setError((e as Error).message);
      } finally {
        setLoading(false);
      }
    })();
  }, [owner, repoName, installationId, connectionId]);

  useEffect(() => {
    const folder = repoPath.trim();
    const detect = (path: string | undefined, onDone: () => void) =>
      getGithubRepoDetail(owner, repoName, path, installationId, connectionId)
        .then((d) => {
          setDetail(d);
          setDeployKind(d.kind);
          onDone();
        })
        .catch(() => setFolderStatus('not-found'));
    if (!folder) {
      detect(undefined, () => setFolderStatus('idle'));
      return;
    }
    setFolderStatus('checking');
    const t = setTimeout(() => detect(folder, () => setFolderStatus('ok')), 400);
    return () => clearTimeout(t);
  }, [owner, repoName, repoPath, installationId, connectionId]);

  // Live check: the deploy dialog only creates — an existing ID must be
  // reworded/changed or linked from that project's settings instead.
  useEffect(() => {
    if (fixedProjectId) return;
    const id = projectId.trim();
    if (!id) {
      setIdTaken(false);
      return;
    }
    const t = setTimeout(() => {
      fetch(`/projects/${id}`, { credentials: 'include' })
        .then((res) => setIdTaken(res.ok))
        .catch(() => setIdTaken(false));
    }, 300);
    return () => clearTimeout(t);
  }, [projectId, fixedProjectId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: prefill once per repo/project default
  useEffect(() => {
    if (!projectId || loading) return;
    getProjectGithubConfig(projectId)
      .then((cfg) => {
        if (cfg.repo_full_name === repo.full_name) {
          setBranch(cfg.branch || branch);
          setDeployKind(cfg.deploy_kind);
          setAutoDeploy(cfg.auto_deploy);
          if (cfg.inputs.port) setPort(cfg.inputs.port);
          setRepoPath(cfg.inputs.path ?? '');
        }
      })
      .catch(() => {});
  }, [repo.full_name, detail?.default_branch]);

  const submit = async () => {
    if (!projectId.trim() || !branch || folderStatus === 'not-found') return;
    setBusy(true);
    setError(null);
    try {
      let project: Project;
      if (fixedProjectId) {
        const res = await fetch(`/projects/${fixedProjectId}`, { credentials: 'include' });
        if (!res.ok) throw new Error(`project '${fixedProjectId}' not found`);
        project = (await res.json()) as Project;
      } else {
        const res = await fetch(`/projects/${projectId.trim()}`, { credentials: 'include' });
        if (res.ok) {
          throw new Error(
            `project '${projectId.trim()}' already exists — pick a new ID, or link the repo from that project's settings`,
          );
        }
        project = await createProject(projectId.trim());
      }

      await linkProjectGithub(project.id, {
        repo_full_name: repo.full_name,
        repo_id: repo.id,
        installation_id: installationId,
        connection_id: connectionId,
        branch,
        ...(repoPath.trim() ? { path: repoPath.trim() } : {}),
        ...(commitMessage.trim() ? { commit_message: commitMessage.trim().slice(0, 100) } : {}),
        deploy_kind: deployKind,
        compose_file: detail?.compose_file ?? null,
        auto_deploy: autoDeploy,
        ...(deployKind === 'single' ? { port } : {}),
      });

      showToast(`Linked ${repo.full_name} → ${project.id}`, 'success');

      if (autoDeploy) {
        showToast('Deploy triggered by the workflow commit — watch the GitHub icon on the project card', 'success');
      } else {
        try {
          await dispatchGithubDeploy(project.id);
          showToast('Deploy dispatched — watch the GitHub icon on the project card', 'success');
        } catch (e) {
          showToast(`Linked, but first deploy failed to dispatch: ${(e as Error).message}`, 'error');
        }
      }
      onLinked();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-md mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        <div className="flex items-center gap-2 px-5 py-4 border-b border-slate-700/50 shrink-0">
          <button
            type="button"
            onClick={onBack}
            className="p-1 -ml-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
            aria-label="Back to repo list"
          >
            <ChevronLeft size={16} />
          </button>
          <h2 className="text-sm font-semibold text-slate-100 flex items-center gap-2 truncate">
            <GithubMark size={14} className="shrink-0" />
            {repo.full_name}
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="ml-auto p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        {loading ? (
          <div className="p-8 flex justify-center">
            <Loader2 size={16} className="animate-spin text-slate-500" />
          </div>
        ) : !confirming ? (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setConfirming(true);
            }}
            className="p-5 space-y-4 overflow-y-auto"
          >
            <div>
              <label htmlFor="gh-project-id" className="block text-xs font-medium text-slate-400 mb-1.5">
                Project ID
              </label>
              <input
                id="gh-project-id"
                type="text"
                value={fixedProjectId ?? projectId}
                onChange={(e) => setProjectId(sanitizeProjectId(e.target.value))}
                placeholder="my-app"
                required
                readOnly={!!fixedProjectId}
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors read-only:cursor-default read-only:text-slate-400"
              />
              <p className="text-xs text-slate-500 mt-1">
                {deployKind === 'compose' ? 'Hostname reserved: ' : 'Used as subdomain: '}
                <span className="text-slate-400">
                  {projectId || 'my-app'}.{domain}
                </span>
              </p>
              {idTaken && !fixedProjectId && (
                <p className="text-xs text-amber-300/90 mt-1 flex items-start gap-1.5">
                  <AlertTriangle size={11} className="mt-0.5 shrink-0" />
                  Project ID already exists! Check project card settings to link repo to existing
                  projects.
                </p>
              )}
            </div>

            <div>
              <label htmlFor="gh-path" className="block text-xs font-medium text-slate-400 mb-1.5">
                Folder
              </label>
              <input
                id="gh-path"
                type="text"
                value={repoPath}
                onChange={(e) => setRepoPath(e.target.value)}
                placeholder="repo root — set if the app lives in a subfolder"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
              {folderStatus === 'checking' && (
                <p className="text-xs text-slate-500 mt-1 flex items-center gap-1.5">
                  <Loader2 size={11} className="animate-spin" />
                  Checking {repoPath.trim() || 'folder'}…
                </p>
              )}
              {folderStatus === 'not-found' && (
                <p className="text-xs text-amber-300/90 mt-1 flex items-center gap-1.5">
                  <AlertTriangle size={11} className="shrink-0" />
                  Folder not found on the default branch
                </p>
              )}
              {folderStatus === 'ok' && detail && (
                <p className="text-xs text-slate-500 mt-1 flex items-center gap-1">
                  {detail.compose_file ? (
                    <>
                      <CheckCircle2 size={11} className="text-emerald-400" />
                      <span className="text-slate-400">Compose detected ({detail.compose_file})</span>
                    </>
                  ) : detail.dockerfile_present ? (
                    <>
                      <CheckCircle2 size={11} className="text-emerald-400" />
                      <span className="text-slate-400">Dockerfile detected</span>
                    </>
                  ) : (
                    <span>No Dockerfile — Railpack will auto-detect the build</span>
                  )}
                </p>
              )}
            </div>

            <div>
              <label htmlFor="gh-branch" className="block text-xs font-medium text-slate-400 mb-1.5">
                Branch
              </label>
              <select
                id="gh-branch"
                value={branch}
                onChange={(e) => setBranch(e.target.value)}
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 focus:outline-none focus:border-violet-500/50 transition-colors cursor-pointer"
              >
                {branches.map((b) => (
                  <option key={b} value={b}>
                    {b}
                  </option>
                ))}
              </select>
            </div>

            {detail?.kind === 'compose' && (
              <div>
                <span className="block text-xs font-medium text-slate-400 mb-1.5">Deploy mode</span>
                <div className="grid grid-cols-2 gap-2">
                  {(['compose', 'single'] as const).map((kind) => (
                    <button
                      key={kind}
                      type="button"
                      onClick={() => setDeployKind(kind)}
                      className={`rounded-md border px-3 py-2 text-left transition-colors cursor-pointer ${
                        deployKind === kind
                          ? 'border-violet-500/50 bg-violet-500/10'
                          : 'border-slate-700/50 bg-slate-900/50 hover:border-slate-600'
                      }`}
                    >
                      <span className="block text-xs font-medium text-slate-200">
                        {kind === 'compose'
                          ? `Compose${detail?.compose_file ? ` (${detail.compose_file})` : ''}`
                          : 'Single service'}
                      </span>
                      <span className="block text-xs text-slate-500 mt-0.5">
                        {kind === 'compose'
                          ? 'All services from the compose file'
                          : 'Build one service (Railpack/Dockerfile)'}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            )}

            {deployKind === 'single' && (
              <div>
                <label htmlFor="gh-port" className="block text-xs font-medium text-slate-400 mb-1.5">
                  Internal port
                </label>
                <input
                  id="gh-port"
                  type="number"
                  min={1}
                  max={65535}
                  value={port}
                  onChange={(e) => setPort(Number(e.target.value) || 0)}
                  className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 focus:outline-none focus:border-violet-500/50 transition-colors"
                />
              </div>
            )}

            <div className="flex items-center gap-2">
              <span className="text-xs text-slate-400">Auto-deploy on push</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoDeploy}
                onClick={() => setAutoDeploy((v) => !v)}
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

            <p className="text-xs text-slate-500 flex items-start gap-1.5">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" />
              Builds run on GitHub-hosted runners; private repos count against your Actions minutes.
            </p>

            {error && <p className="text-xs text-red-400">{error}</p>}

            <button
              type="submit"
              disabled={
                busy || !projectId.trim() || !branch || folderStatus === 'not-found' || (idTaken && !fixedProjectId)
              }
              className="w-full py-2.5 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 disabled:hover:bg-violet-600 text-xs font-medium text-white transition-colors cursor-pointer flex items-center justify-center gap-1.5"
            >
              <Rocket size={12} />
              Link & deploy
            </button>
          </form>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
            className="p-5 space-y-4 overflow-y-auto bg-slate-900/40"
          >
            <div className="flex items-center justify-between">
              <span className="text-xs uppercase tracking-wide text-violet-300">Review</span>
              <button
                type="button"
                onClick={() => setConfirming(false)}
                className="text-xs text-slate-500 hover:text-slate-300 transition-colors cursor-pointer flex items-center gap-1"
              >
                <ChevronLeft size={11} />
                Edit
              </button>
            </div>

            <div className="rounded-md border border-dashed border-slate-600/70 p-3 space-y-2">
              <p className="text-xs text-slate-500">This will, on GitHub:</p>
              <div className="space-y-1.5">
                <div className="flex items-center gap-2 text-xs">
                  <GithubMark size={11} className="text-slate-600 shrink-0" />
                  <span className="text-slate-300 truncate">
                    Commit <span className="font-mono">.github/workflows/litebin-deploy.yml</span>
                  </span>
                </div>
                <div className="flex items-center gap-2 text-xs text-slate-500">
                  <span className="w-[11px] shrink-0" />
                  to <span className="text-slate-400">{branch}</span>
                  {autoDeploy && <span className="text-slate-500">· auto-deploy on push</span>}
                </div>
                <div className="flex items-center gap-2 text-xs text-slate-500">
                  <KeyRound size={11} className="text-slate-600 shrink-0" />
                  Set secrets <span className="font-mono">L8B_SERVER</span>,{' '}
                  <span className="font-mono">L8B_TOKEN</span>
                </div>
              </div>
            </div>

            <div className="rounded-md border border-slate-700/50 bg-slate-900/50 p-3 space-y-2">
              <label htmlFor="gh-commit-msg" className="block text-xs font-medium text-slate-400">
                Commit message
              </label>
              <input
                id="gh-commit-msg"
                type="text"
                value={commitMessage}
                onChange={(e) => setCommitMessage(e.target.value)}
                maxLength={100}
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
              {error && <p className="text-xs text-red-400">{error}</p>}
            </div>

            <button
              type="submit"
              disabled={busy}
              className="w-full py-2.5 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 text-xs font-medium text-white transition-colors cursor-pointer flex items-center justify-center gap-1.5"
            >
              {busy ? <Loader2 size={12} className="animate-spin" /> : <Rocket size={12} />}
              {busy ? 'Linking…' : 'Confirm & deploy'}
            </button>
          </form>
        )}
      </div>
    </div>
  );
}
